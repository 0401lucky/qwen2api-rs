FROM alpine:latest

# 下载 Mihomo (Clash Meta)
ADD https://github.com/MetaCubeX/mihomo/releases/download/v1.19.10/mihomo-linux-amd64-v1.19.10.gz /tmp/mihomo.gz
RUN gunzip /tmp/mihomo.gz && chmod +x /tmp/mihomo && mv /tmp/mihomo /usr/local/bin/mihomo

COPY config.yaml /etc/mihomo/config.yaml
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 7890 9090

ENTRYPOINT ["/entrypoint.sh"]
