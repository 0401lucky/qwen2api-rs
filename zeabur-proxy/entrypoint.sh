#!/bin/sh
set -e

# 用环境变量替换 config 中的占位符
if [ -n "$SUB_URL" ]; then
    sed -i "s|\${SUB_URL}|${SUB_URL}|g" /etc/mihomo/config.yaml
else
    echo "ERROR: SUB_URL 环境变量未设置！"
    exit 1
fi

exec mihomo -f /etc/mihomo/config.yaml
