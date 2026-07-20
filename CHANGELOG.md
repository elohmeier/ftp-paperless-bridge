# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/)
and this project adheres to [Semantic Versioning](http://semver.org/).

<!-- next-header -->

## [Unreleased] - ReleaseDate

## [0.3.3] - 2026-07-20
- Reject FTP logins while Paperless is unavailable so scanners can block jobs before scanning
- Reject newly-started uploads with a transient FTP error when the cached health status is unhealthy
- Restore FTP access automatically after Paperless recovers
- Add short connection and request timeouts to Paperless health checks

## [0.3.2] - 2026-04-09
- Retry the startup Paperless API validation with exponential backoff

## [0.2.1] - 2025-08-18
- Actually make use of the set passive mode ports

## [0.2.0] - 2025-08-18
- Enable passive mode in addition to active mode

## [0.1.0] - 2025-07-02
- Initial version

<!-- next-url -->
[Unreleased]: https://github.com/svenstaro/ftp-paperless-bridge/compare/v0.3.3...HEAD
[0.3.3]: https://github.com/svenstaro/ftp-paperless-bridge/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/svenstaro/ftp-paperless-bridge/compare/v0.3.1...v0.3.2
[0.2.1]: https://github.com/svenstaro/ftp-paperless-bridge/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/svenstaro/ftp-paperless-bridge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/svenstaro/ftp-paperless-bridge/compare/...v0.1.0
