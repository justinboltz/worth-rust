use anyhow::Context;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "worth=info,tower_http=info".into()),
        )
        .init();
    let state = worth::web::AppState::from_env().await?;
    let bind = std::env::var("BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3001".into());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .context("binding Worth server")?;
    tracing::info!(%bind, "Worth is ready");
    let worker = tokio::spawn(worth::plaid::worker(state.clone()));
    axum::serve(listener, worth::web::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    worker.abort();
    Ok(())
}
