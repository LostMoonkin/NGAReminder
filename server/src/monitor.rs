use crate::config_holder::ConfigHolder;
use crate::crawler::Crawler;
use crate::model::config::MonitoredThread;
use crate::model::nga_thread::NGAPost;
use crate::notifier::{BarkNotifier, ConsoleNotifier, Notifier};
use chrono::{DateTime, Datelike, Duration, Local, Weekday};
use std::cmp::max;
use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const DEFAULT_POST_PER_PAGE: u64 = 20;
const DEFAULT_THREAD_CHECK_INTERVAL: u64 = 300;
const WEEKDAYS: [&str; 5] = ["monday", "tuesday", "wednesday", "thursday", "friday"];
const WEEKENDS: [&str; 2] = ["saturday", "sunday"];

pub struct NGAMonitor {
    config_holder: Arc<ConfigHolder>,
    crawler: Crawler,
    last_check_map: HashMap<u64, DateTime<Local>>,
    notifiers: Vec<Box<dyn Notifier>>,
}

impl NGAMonitor {
    pub async fn new(config_holder: Arc<ConfigHolder>, crawler: Crawler) -> Self {
        let notifier_config = config_holder.get_notifier_config().await;
        let mut notifiers: Vec<Box<dyn Notifier>> = Vec::new();
        if let Some(bark_config) = notifier_config.bark {
            notifiers.push(Box::new(BarkNotifier::new(bark_config)));
        }
        if let Some(console_config) = notifier_config.console {
            notifiers.push(Box::new(ConsoleNotifier::new(console_config)));
        }
        Self {
            config_holder,
            crawler,
            last_check_map: HashMap::new(),
            notifiers,
        }
    }

    async fn get_monitor_config(&self) -> crate::model::config::MonitorConfig {
        self.config_holder.get_monitor_config().await
    }

    async fn get_crawler_config(&self) -> crate::model::config::CrawlerConfig {
        self.config_holder.get_crawler_config().await
    }

    async fn update_post_last_seen(&self, tid_to_post_number: &HashMap<u64, u64>) -> Result<(), Box<dyn Error>> {
        self.config_holder.update_post_last_seen(tid_to_post_number).await
    }

    pub async fn run(&mut self) {
        let config = self.get_monitor_config().await;
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(config.monitor_duration));
        // Define behavior if the system lags (Skip missed ticks to catch up)
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        println!("STARTED: NGA Monitor (Every {}s)", config.monitor_duration);
        loop {
            interval.tick().await;
            println!("Start check threads.");
            let mut tid_to_max_post_number = HashMap::new();
            let monitored_threads = self.get_monitor_config().await.monitored_threads;
            
            for thread in monitored_threads {
                let last_check = self.last_check_map.get(&thread.tid);
                let mut need_check = false;
                match last_check {
                    None => need_check = true,
                    Some(last_check_date) => {
                        // get next check time for thread
                        let check_interval = self.get_check_interval(&thread);
                        let now = Local::now();
                        let next_check_date = *last_check_date + check_interval;
                        if now.gt(&next_check_date) {
                            need_check = true;
                        }
                    }
                }
                if need_check {
                    let res = self.check_thread(&thread).await;
                    match res {
                        Ok(max_post_number) => {
                            println!(
                                "Check thread finished, tid: {}, max_post_number: {}",
                                thread.tid, max_post_number
                            );
                            _ = tid_to_max_post_number.insert(thread.tid, max_post_number)
                        }
                        Err(err) => {
                            println!("Monitor thread failed ({}): {}", thread.tid, err);
                        }
                    }
                    self.last_check_map.insert(thread.tid, Local::now());
                }
            }
            
            let res = self.update_post_last_seen(&tid_to_max_post_number).await;
            if res.is_err() {
                println!("Update post last seen failed: {}", res.err().unwrap());
            }
        }
    }

    pub async fn check_thread(
        &self,
        thread_config: &MonitoredThread,
    ) -> Result<u64, Box<dyn Error>> {
        let monitor_config = self.get_monitor_config().await;
        println!(
            "Checking thread: tid={}, last_seen_post_number={}",
            thread_config.tid, thread_config.last_seen_post_number
        );

        let last_seen_page = thread_config.last_seen_post_number / DEFAULT_POST_PER_PAGE + 1;
        let crawler_config = self.get_crawler_config().await;
        let cur_page = self
            .crawler
            .fetch_thread_with_page(
                thread_config.tid,
                last_seen_page,
                &crawler_config,
            )
            .await?;
        // Limit parallel task nums.
        let task_semaphore = Arc::new(Semaphore::new(
            monitor_config.fetch_posts_parallel_limit as usize,
        ));
        // Arc crawler and config
        let crawler = Arc::new(self.crawler.clone());
        let crawler_config = Arc::new(crawler_config);
        let mut tasks = JoinSet::new();
        let tid = thread_config.tid;
        for page_num in (last_seen_page + 1)..=cur_page.total_pages {
            // async fetch thread pages.
            let semaphore_cloned = task_semaphore.clone();
            let crawler_cloned = crawler.clone();
            let crawler_config_cloned = crawler_config.clone();
            tasks.spawn(async move {
                let _permit = semaphore_cloned.acquire_owned().await;
                println!("Starting fetch thread #{}", page_num);
                let res = crawler_cloned
                    .fetch_thread_with_page(tid, page_num, &crawler_config_cloned)
                    .await;
                match res {
                    Ok(data) => Ok(data),
                    Err(err) => {
                        println!(
                            "Error occurred during fetch thread data: tid={}, err={}",
                            tid, err
                        );
                        Err(err.to_string())
                    }
                }
            });
        }
        // join fetch results
        let mut task_results = vec![];
        task_results.push(cur_page);
        while let Some(res) = tasks.join_next().await {
            match res {
                Ok(thread_res) => match thread_res {
                    Ok(thread_data) => {
                        task_results.push(thread_data);
                    }
                    Err(_) => {}
                },
                Err(err) => {
                    eprintln!("Error join fetch thread tasks: {}", err);
                }
            }
        }

        // parse thread page data
        task_results.sort_by(|a, b| a.current_page.cmp(&b.current_page));
        let mut max_post_number = 0;
        for thread in task_results {
            let posts = thread.posts;
            for post in posts {
                max_post_number = max(max_post_number, post.post_number);
                if thread_config
                    .author_notification
                    .contains(&post.author.author_uid)
                    && post.post_number > thread_config.last_seen_post_number
                {
                    println!("Collect notify post: tid={}, pid={}", post.tid, post.pid);
                    // notify
                    self.send_notification(post).await
                }
            }
        }
        Ok(max_post_number)
    }

    async fn send_notification(&self, post: NGAPost) {
        let title = format!("New Post: {}", post.thread_title);
        let message = format!(
            "{} (#{}):\n{}...",
            post.author.author_name, post.post_number, post.content
        );
        let extra = HashMap::from([(
            "url".to_string(),
            format!(
                "https://nga.178.com/read.php?tid={}&page={}#pid{}Anchor",
                post.tid, post.page, post.pid
            ),
        )]);
        for notifier in &self.notifiers {
            let _success = notifier
                .send_notification(&title, &message, Some(extra.clone()))
                .await;
        }
    }

    fn get_check_interval(&self, thread_config: &MonitoredThread) -> Duration {
        let default_interval = if thread_config.check_interval == 0 {
            DEFAULT_THREAD_CHECK_INTERVAL
        } else {
            thread_config.check_interval
        };
        if thread_config.check_schedule.is_none() {
            return Duration::seconds(default_interval as i64);
        }
        let now = Local::now();
        let now_weekday = now.weekday();
        let schedules = thread_config.check_schedule.clone().unwrap();
        for schedule in schedules {
            // get expanded days
            let expanded_days = expand_days(thread_config.tid, schedule.days);
            // Check day match
            if !expanded_days.contains(&now_weekday) {
                continue;
            }
            // Check time match
            let now_hour = now.format("%H:%M").to_string();
            if is_time_range(now_hour, schedule.start_time, schedule.end_time) {
                return Duration::seconds(schedule.interval as i64);
            }
        }
        Duration::seconds(default_interval as i64)
    }
}

fn is_time_range(current_time: String, start: String, end: String) -> bool {
    if start <= end {
        // Normal range (e.g., 09:00 to 18:00)
        current_time >= start && current_time < end
    } else {
        // Wrap-around (e.g., 22:00 to 06:00)
        current_time >= start || current_time < end
    }
}

fn expand_days(tid: u64, str_weekdays: Vec<String>) -> Vec<Weekday> {
    let mut res: Vec<Weekday> = vec![];
    for val in str_weekdays {
        let day_str = val.clone().to_lowercase();
        let expand = match day_str.as_str() {
            "weekdays" => WEEKDAYS.to_vec(),
            "weekends" => WEEKENDS.to_vec(),
            _ => [day_str.as_str()].to_vec(),
        };
        for v in expand {
            match Weekday::from_str(v) {
                Ok(day) => res.push(day),
                Err(err) => {
                    println!("Error parsing weekday: tid={}, err={}", tid, err);
                }
            }
        }
    }
    res
}
