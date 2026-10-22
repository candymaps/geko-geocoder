# Contribuir a geko

## Licencia

geko se publica con la licencia [AGPL-3.0](LICENSE), y cada contribución se acepta con esa misma licencia, sin firmar nada aparte: conservas la autoría de tu código. Solo los colaboradores del repositorio pueden abrir pull requests; si quieres contribuir, pide que te inviten. Si contribuyes en nombre de tu empresa, o parte del código no es tuyo, dilo en el PR.

## Ramas

Todo desarrollo sale de `dev` a una rama propia (`feature/<nombre>`, `bugfix/<nombre>`…) y vuelve a `dev` con un pull request. A `main` solo se llega con un pull request de `dev` a `main`.

## Comprobaciones

Cada PR pasa en la CI lo que puede romper de `ci/test.sh` (`ci/affected.sh`): el código, los contratos y el golden de los países que puede cambiar, con sus instrucciones; nada si solo cambia documentación (un comentario del bot lo dice). No se puede fusionar hasta que pase. La latencia y España completa no se miden en la CI: si tu cambio toca el motor, mídelo en local con `make test-regression`. Los ficheros de la licencia, de la documentación del repositorio (`README.md`, `CONTRIBUTING.md`, `AGENTS.md`, `CLAUDE.md`), de la CI (`.github/`, `ci/`) y de la configuración de las comprobaciones (`deny.toml`, `clippy.toml`, `.cargo/`, `rust-toolchain.toml`) solo los cambia el propietario: una PR que los toque no se puede fusionar; si crees que hace falta cambiar alguno, abre una issue. Antes de abrirlo, puedes pasarlo en local con `make test` (o solo lo que tu cambio puede romper, con `make test-changed`). Las reglas del proyecto están en [AGENTS.md](AGENTS.md), y la especificación en [PROJECT.md](docs/PROJECT.md).
