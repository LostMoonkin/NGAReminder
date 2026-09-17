package content

// 通知与内容页面共用摘要入口；未知标记保留，避免丢失正文。
func Summary(body string, limit int) string {
	text := Text(body)
	runes := []rune(text)
	if len(runes) > limit {
		return string(runes[:limit]) + "…"
	}
	return text
}
