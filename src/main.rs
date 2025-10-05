//! Bootstraps a PostgreSQL data directory as the `nobody` user.
//!
//! Configuration is provided via environment variables parsed by
//! [`OrthoConfig`](https://github.com/leynos/ortho-config). The binary exits
//! with status code `0` on success and `1` on error.

fn main() -> color_eyre::eyre::Result<()> {
    postgres_setup_unpriv::run()
}
