FROM golang:1.27.0-alpine3.23 AS build

ENV CGO_ENABLED=0 GOTOOLCHAIN=local
WORKDIR /src

COPY go.mod go.sum ./
RUN go mod download

COPY cmd ./cmd
COPY internal ./internal
COPY web ./web
RUN go build -mod=readonly -trimpath -o /out/nga-reminder ./cmd/server

FROM alpine:3.23

# HTTPS 使用系统 CA；管理页和时区数据已嵌入 Go 二进制。
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/nga-reminder /usr/local/bin/nga-reminder

WORKDIR /app
EXPOSE 8989
ENTRYPOINT ["/usr/local/bin/nga-reminder"]
