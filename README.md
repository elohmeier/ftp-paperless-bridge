# ftp-paperless-bridge

Present a FTP server to your network scanner and forward anything received to paperless-ngx

The bridge checks Paperless every five seconds. While Paperless is unavailable, FTP logins are
rejected so scanners that test their destination before scanning can block the job and show an
error. Logins are enabled again automatically when Paperless recovers. An upload admitted just
before an outage is detected is rejected with a transient FTP error before its data is read.

When a spool directory is configured, a document that was already received when Paperless becomes
unavailable is saved for later delivery and reported as successful to the scanner. This avoids the
duplicate documents that could result from both spooling and asking the scanner to retry.

## Run

```shell
cp .env.example .env
# Fill out .env with your info
podman run --init -it --env-file .env ghcr.io/svenstaro/ftp-paperless-bridge:latest
```

## Develop

```shell
cp .env.example .env
# Fill out .env with your info
just run
```
