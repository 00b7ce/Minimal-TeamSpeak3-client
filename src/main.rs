use anyhow::Result;
use futures::prelude::*;
use tsclientlib::{Connection, DisconnectOptions, StreamItem};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "192.168.10.8".to_string());
    let nickname = args.next().unwrap_or_else(|| "mekabu".to_string());

    println!("{address} に {nickname} として接続します...");

    // identityを指定しない場合は毎回新しい鍵が生成される(サーバからは別人扱い)。
    // 固定したくなったら設定ファイルに保存したものをConnection::buildに渡す。
    let mut con = Connection::build(address).name(nickname).connect()?;

    // 最初の状態同期(BookEvents)が届くまで待つ
    let r = con
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await;
    if let Some(r) = r {
        r?;
    }

    {
        let state = con.get_state()?;
        println!("接続完了: サーバ名 = {}", state.server.name);
        println!("--- チャンネル/クライアント一覧 ---");
        for (id, channel) in &state.channels {
            println!("[{}] {}", id.0, channel.name);
            for client in state.clients.values().filter(|c| c.channel == *id) {
                println!("    - {}", client.name);
            }
        }
        println!("-----------------------------------");
    }

    println!("イベントを監視中 (Ctrl+C で切断して終了)");
    let mut events = con.events();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            item = events.next() => match item {
                Some(Ok(StreamItem::BookEvents(events))) => {
                    for event in &events {
                        println!("[event] {event:?}");
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    eprintln!("接続エラー: {e}");
                    return Err(e.into());
                }
                None => {
                    println!("サーバから切断されました");
                    return Ok(());
                }
            }
        }
    }
    drop(events);

    println!("切断します...");
    con.disconnect(DisconnectOptions::new())?;
    // 切断完了(ストリーム終了)まで待つ
    con.events().for_each(|_| future::ready(())).await;
    println!("切断完了");

    Ok(())
}
