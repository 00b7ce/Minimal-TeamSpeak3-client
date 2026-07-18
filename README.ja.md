# Minimal TeamSpeak 3 Client

[English](README.md) | [日本語](README.ja.md)

StreamDeck連携用HTTP API・システムトレイ常駐・自動起動を備えた、Windows向けのミニマルなTeamSpeak 3クライアントです。Rust製。

公式のTeamSpeakクライアントは多機能ですが、「自宅のTS3サーバで低遅延ボイスチャットをしたいだけ」という用途には過剰です。このクライアントはその一点に集中し、それ以外(連絡先、チャットUI、バッジ等)をすべて削ぎ落としました。接続して、話して、あとはトレイに引っ込んでいる——それだけのクライアントです。

<p align="center"><img src="docs/main_window.png" alt="メインウィンドウ" width="380"></p>

> **注意**: 本ソフトウェアは非公式クライアントであり、TeamSpeak Systems GmbHとは無関係です。TeamSpeak 3プロトコルの実装には [tsclientlib](https://github.com/ReSpeak/tsclientlib) を使用しています。

## 機能

- **ミニマルなUI** — プロファイル選択、接続ボタン、チャンネルツリー。以上。
- **接続プロファイル** — 複数サーバを登録可能。プロファイルごとに表示名・アドレス・ニックネームを持ちます。
- **音声** — Opusボイス。入出力デバイスを選択でき、送信モードは3種類:
  - 常時送信
  - ボイス検出(閾値スライダー+リアルタイム入力レベルメーター)
  - プッシュトゥトーク(グローバルキー監視 — ゲームにフォーカスがあっても有効)
- **相手ごとの音量調整** — チャンネルツリーのユーザー名を右クリックして再生音量を調整(0〜200%)。ニックネーム単位で保存され、再接続後も引き継がれます。
- **StreamDeck・自動化との連携** — ローカルHTTP API(127.0.0.1のみ)で接続/切断/ミュートを操作可能。HTTPリクエストを送れるStreamDeckプラグインやスクリプトから利用できます。
- **システムトレイ常駐** — ウィンドウを閉じるとトレイに格納。トレイメニューから開く/終了。
- **自動起動** — Windows起動時にトレイ格納状態で自動起動(設定でON/OFF)。
- **サウンドフィードバック** — 自分の接続/切断、他ユーザーの入退室で効果音が鳴ります。画面を見ずにStreamDeck操作の結果が分かります。
- **堅牢な接続処理** — 接続断からの自動再接続、ハンドシェイクタイムアウト、診断用ログファイル。
- **日本語/英語UI** — 既定ではOSの言語に追従。設定で切替可能。

## 動作環境

- Windows 10/11
- 接続先のTeamSpeak 3サーバ

## ソースからのビルド

必要なもの:

- [Rust](https://rustup.rs/)(stable、MSVCツールチェーン)
- Visual Studio Build Tools(C++ワークロード — MSVCリンカのため)
- [CMake](https://cmake.org/)(同梱のOpusコーデックのビルドに使用)

```
git clone https://github.com/00b7ce/Minimal-TeamSpeak3-client.git
cd Minimal-TeamSpeak3-client
cargo build --release
```

`target/release/ts3-client.exe` が生成されます。フォント同梱の単一exeなので、好きな場所に配置して使えます。

## 使い方

1. `ts3-client.exe` を起動
2. **⚙ 設定** を開き、プロファイル(プロファイル名・サーバアドレス・ニックネーム)を登録
3. **接続** をクリック

プロファイル・音声デバイス・相手ごとの音量などの設定は `%APPDATA%\ts3-client\config.toml` に保存されます。診断用ログは `%APPDATA%\ts3-client\ts3-client.log` に出力されます。

### 起動オプション

| オプション | 説明 |
|---|---|
| `--minimized` | トレイに格納された状態で起動(自動起動で使用) |
| `--lang ja` / `--lang en`(短縮形 `--ja` / `--en`) | このセッションのUI言語を指定 |
| `--autoconnect` | 起動と同時に選択中のプロファイルへ接続 |

### 送信モード

| モード | 動作 |
|---|---|
| 常時送信 | 接続中は常にマイク音声を送信 |
| ボイス検出 | 入力レベルが閾値を超えている間+300msだけ送信。内蔵レベルメーターで閾値を調整できます |
| プッシュトゥトーク | キーを押している間だけ送信。キー状態はグローバルに監視されるため、ゲーム中でも有効。マウスサイドボタンも選択可能 |

## StreamDeck / HTTP API

`http://127.0.0.1:9871/api/` で待ち受けます(ポートは `config.toml` で変更可、ループバックのみ)。全エンドポイントがGET/POST両対応なので、シンプルなHTTPリクエストプラグインやブラウザからも操作できます。

| エンドポイント | 動作 |
|---|---|
| `/api/connect` | 選択中のプロファイルへ接続 |
| `/api/connect/{プロファイル名}` | 名前を指定して接続 |
| `/api/disconnect` | 切断 |
| `/api/mute/on` `/api/mute/off` `/api/mute/toggle` | マイクミュート操作 |
| `/api/status` | 接続状態とミュート状態をJSONで返す |

例:

```
curl -X POST http://127.0.0.1:9871/api/connect
curl http://127.0.0.1:9871/api/status
{"status":"connected","server_name":"My Server","error":null,"muted":false}
```

接続と切断で異なるチャイムが鳴るため、画面を見ずにStreamDeck操作の結果を確認できます。

## 謝辞

- [tsclientlib](https://github.com/ReSpeak/tsclientlib) — TeamSpeak 3プロトコル実装
- [egui / eframe](https://github.com/emilk/egui) — UIフレームワーク
- [cpal](https://github.com/RustAudio/cpal) — オーディオI/O
- 同梱フォント: [Inter](https://rsms.me/inter/)・[Noto Sans JP](https://fonts.google.com/noto/specimen/Noto+Sans+JP)(いずれもSIL Open Font License、`assets/` 参照)

## ライセンス

[Apache License, Version 2.0](LICENSE-APACHE) または [MITライセンス](LICENSE-MIT) のいずれか(選択可)でライセンスされます。
