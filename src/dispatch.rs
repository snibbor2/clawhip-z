use tokio::sync::mpsc;

use crate::Result;
use crate::discord::DiscordClient;
use crate::events::IncomingEvent;
use crate::router::Router;

pub struct Dispatcher {
    rx: mpsc::Receiver<IncomingEvent>,
    router: Router,
    discord: DiscordClient,
}

impl Dispatcher {
    pub fn new(rx: mpsc::Receiver<IncomingEvent>, router: Router, discord: DiscordClient) -> Self {
        Self {
            rx,
            router,
            discord,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        while let Some(event) = self.rx.recv().await {
            let deliveries = match self.router.resolve(&event).await {
                Ok(deliveries) => deliveries,
                Err(error) => {
                    eprintln!(
                        "clawhip dispatcher failed to resolve {}: {error}",
                        event.canonical_kind()
                    );
                    continue;
                }
            };

            for delivery in deliveries {
                if let Err(error) = self.discord.send(&delivery.target, &delivery.content).await {
                    eprintln!(
                        "clawhip dispatcher delivery failed to {:?}: {error}",
                        delivery.target
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::{AppConfig, RouteRule};

    #[tokio::test]
    async fn dispatcher_stops_cleanly_when_channel_closes() {
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let router = Router::new(Arc::new(AppConfig::default()));
        let discord = DiscordClient::from_config(Arc::new(AppConfig::default())).unwrap();
        let mut dispatcher = Dispatcher::new(rx, router, discord);

        dispatcher.run().await.unwrap();
    }

    #[tokio::test]
    async fn dispatcher_continues_after_webhook_failure() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::time::{Duration, timeout};

        async fn spawn_webhook(status: &str) -> (String, tokio::task::JoinHandle<String>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let status_line = status.to_string();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0_u8; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let response = format!("HTTP/1.1 {status_line}\r\ncontent-length: 0\r\n\r\n");
                stream.write_all(response.as_bytes()).await.unwrap();
                req
            });

            (format!("http://{addr}/webhook"), server)
        }

        let (failing_webhook, failing_server) = spawn_webhook("500 Internal Server Error").await;
        let (successful_webhook, successful_server) = spawn_webhook("204 No Content").await;
        let config = AppConfig {
            routes: vec![
                RouteRule {
                    event: "tmux.keyword".into(),
                    filter: Default::default(),
                    channel: None,
                    webhook: Some(failing_webhook),
                    mention: None,
                    allow_dynamic_tokens: false,
                    format: None,
                    template: Some("first".into()),
                },
                RouteRule {
                    event: "tmux.keyword".into(),
                    filter: Default::default(),
                    channel: None,
                    webhook: Some(successful_webhook),
                    mention: None,
                    allow_dynamic_tokens: false,
                    format: None,
                    template: Some("second".into()),
                },
            ],
            ..AppConfig::default()
        };
        let (tx, rx) = mpsc::channel(1);
        let router = Router::new(Arc::new(config));
        let discord = DiscordClient::from_config(Arc::new(AppConfig::default())).unwrap();
        let mut dispatcher = Dispatcher::new(rx, router, discord);
        let task = tokio::spawn(async move { dispatcher.run().await.unwrap() });

        tx.send(IncomingEvent::tmux_keyword(
            "issue-24".into(),
            "error".into(),
            "boom".into(),
            None,
        ))
        .await
        .unwrap();
        drop(tx);

        task.await.unwrap();
        let failing_request = timeout(Duration::from_secs(2), failing_server)
            .await
            .unwrap()
            .unwrap();
        let successful_request = timeout(Duration::from_secs(2), successful_server)
            .await
            .unwrap()
            .unwrap();
        assert!(failing_request.contains("\"content\":\"first\""));
        assert!(successful_request.contains("\"content\":\"second\""));
    }
}
