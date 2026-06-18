FROM metacubex/mihomo:latest

COPY config.yaml /etc/mihomo/config.yaml
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 7890 9090

ENTRYPOINT ["/entrypoint.sh"]
