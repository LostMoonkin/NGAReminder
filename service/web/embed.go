package web

import "embed"

// Templates 将管理页随服务端一起编译，运行时无需读取源码目录。
//
//go:embed templates/*.html
var Templates embed.FS

// Static 保存管理台样式与渐进增强脚本。
//
//go:embed static/*
var Static embed.FS
