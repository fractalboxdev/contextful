FROM alpine:3.22
RUN apk add --no-cache ca-certificates \
 && addgroup -S contextful \
 && adduser -S -G contextful contextful \
 && mkdir -p /work \
 && chown contextful:contextful /work
# `contextful` is the x86_64-unknown-linux-musl `contextful-full` release binary, copied in by the operator.
COPY --chmod=0755 contextful entrypoint.sh /usr/local/bin/
USER contextful
WORKDIR /work
EXPOSE 8080
CMD ["/usr/local/bin/entrypoint.sh"]
