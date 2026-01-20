# NGA Reminder Documentation

## Overview

This directory contains comprehensive documentation for the NGA BBS crawler and monitoring system.

---

## Documentation Index

### 📚 **Getting Started**

1. **[GETTING_STARTED.md](GETTING_STARTED.md)** - Start here!
   - Complete system overview
   - Quick start guide
   - Common workflows
   - Troubleshooting

2. **[PROJECT_SUMMARY.md](PROJECT_SUMMARY.md)** - Project overview
   - Architecture diagram
   - All features at a glance
   - File structure
   - Performance metrics

---

### 🔍 **Monitoring System**

3. **[MONITOR.md](MONITOR.md)** - Monitoring guide
   - All monitoring commands
   - Use cases and examples
   - Event logging
   - Integration tips

4. **[CONFIG_MONITOR.md](CONFIG_MONITOR.md)** - Configuration reference
   - `monitored_threads` format
   - Field descriptions
   - Configuration examples
   - Workflow guide

5. **[MONITORING_IMPROVEMENTS.md](MONITORING_IMPROVEMENTS.md)** - Technical deep dive
   - How `vrows` comparison works
   - Smart page fetching algorithm
   - Performance analysis
   - Example scenarios

6. **[PER_THREAD_INTERVALS.md](PER_THREAD_INTERVALS.md)** - Check interval guide
   - Per-thread scheduling
   - How intervals work
   - Best practices
   - Configuration examples

---

### 🛠️ **Technical Documentation**

7. **[DATABASE.md](DATABASE.md)** - Database reference
   - Schema documentation
   - Common SQL queries
   - Python API examples
   - Maintenance tips

8. **[API_ANALYSIS.md](API_ANALYSIS.md)** - NGA API structure
   - Response format analysis
   - Field descriptions
   - Use cases
   - Data extraction tips

9. **[RATE_LIMITING.md](RATE_LIMITING.md)** - Rate limiting guide
   - Configuration options
   - Performance impact
   - Best practices
   - Troubleshooting

10. **[CHANGELOG.md](CHANGELOG.md)** - Multi-threading update
    - Implementation details
    - Performance improvements
    - Migration notes

---

## Quick Access

### By Topic

**Want to start monitoring?**
→ [GETTING_STARTED.md](GETTING_STARTED.md)

**Setting up config file?**
→ [CONFIG_MONITOR.md](CONFIG_MONITOR.md)

**Understanding how detection works?**
→ [MONITORING_IMPROVEMENTS.md](MONITORING_IMPROVEMENTS.md)

**Need database queries?**
→ [DATABASE.md](DATABASE.md)

**Configuring rate limits?**
→ [RATE_LIMITING.md](RATE_LIMITING.md)

**Check intervals not working?**
→ [PER_THREAD_INTERVALS.md](PER_THREAD_INTERVALS.md)

---

## Contributing

When adding new documentation:
1. Create markdown file in this directory
2. Add entry to this README
3. Link from main README.md if relevant
4. Use consistent formatting

---

## Feedback

If documentation is unclear or missing information, please let us know!
