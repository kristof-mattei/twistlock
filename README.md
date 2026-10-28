# Twistlock

A custom library to interact with the docker daemon

# Features

- Connect over TCP
- Connect over a Unix socket (Unix only)
- Connect over a named pipe (Windows only)
- Resolve the host from `DOCKER_HOST` like the Docker CLI
- Connect over TLS, configured from `DOCKER_TLS_VERIFY`, `DOCKER_TLS` and `DOCKER_CERT_PATH` like the Docker CLI

## License

MIT, see [LICENSE](./LICENSE)

`SPDX-License-Identifier: MIT`
