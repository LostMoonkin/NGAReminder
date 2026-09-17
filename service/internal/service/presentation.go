package service

func StatusText(value string) string {
	if label, ok := map[string]string{"unconfigured": "未配置", "valid": "可用", "ready": "可采集", "auth_paused": "认证暂停", "missing": "主题不存在",
		"cancelled": "已取消", "pending": "待发送", "sent": "已发送",
		"awaiting_confirmation": "等待确认", "preparing": "准备验证码", "awaiting_captcha": "等待验证码", "submitting": "提交登录", "validating_cookie": "校验候选 Cookie", "expired": "已过期",
		"disabled": "已关闭", "connecting": "连接中", "connected": "已连接", "reconnecting": "重连中", "gap_recovery": "楼层补偿", "manual": "手动", "automatic": "自动", "skipped_no_fetch": "免拉取，本轮跳过",
		"running": "运行中", "success": "成功", "failed": "失败", "interrupted": "中断", "skipped_pending": "待审核，本轮跳过", "skipped_busy": "服务器忙，本轮跳过"}[value]; ok {
		return label
	}
	return value
}
