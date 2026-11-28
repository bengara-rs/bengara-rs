# 版と互換性

bengara の版の付け方と、版を上げても壊さないと約束する範囲です。Laravel の「Release Notes」に当たります。

- 変更の中身は [CHANGELOG.md](../CHANGELOG.md) にあります。
- いまの版は 0.1.0-alpha.1 です。[crates.io](https://crates.io/crates/bengara) に出しています。
- 出すクレートは `bengara`・`bengara-build`・`bengara-macros` の 3 つです。いつも同じ版で出します。

## 版の付け方

版は `major.minor.patch` の 3 つの数です。

| 時期       | 壊す変更で上げる所 | 壊さない変更で上げる所 |
|------------|--------------------|------------------------|
| 0.x の間   | minor（0.1 → 0.2） | patch（0.1.0 → 0.1.1） |
| 1.0 から先 | major（1.x → 2.0） | minor か patch         |

### 試験版（alpha）

`-alpha.1` のように `-` の付いた版は試験版です。

- 試験版の間は、下の「壊さない約束」をしません。壊す変更でも `-alpha.1` → `-alpha.2` と上げます。
- Cargo は、`bengara = "0.1"` という指定に試験版を選びません。試すときは版をそのまま書きます。

```toml
[dependencies]
bengara = { version = "0.1.0-alpha.1", features = ["sqlite"] }

[build-dependencies]
bengara-build = "0.1.0-alpha.1"
```

- `cargo run -- init` は、試験版のときは版をそのまま `Cargo.toml` に書きます。
- `0.1.0` を出したら、`"0.1"` と書けば選ばれるようになります。

- 「壊す変更」は、下の「壊さない約束の範囲」を変えるものです。
- Cargo は `0.1` と書いた依存を、0.1.x の中だけで上げます。
  `bengara = "0.1"` と書いておけば、壊れる版には上がりません。

## 壊さない約束の範囲

| 範囲                  | 例                                                                          |
|-----------------------|-----------------------------------------------------------------------------|
| 公開の項目            | `bengara::…` と `bengara::prelude` から使える型・関数・マクロ               |
| 文書に書いた動き      | この `docs/` に書いた動き                                                   |
| `.env` の名前と意味   | `APP_KEY`・`DB_CONNECTION`・`LOG_CHANNEL` など                              |
| 機能フラグの名前      | `sqlite`・`websocket` など                                                  |
| コマンド              | `cargo artisan …` の名前と引数                                              |
| `init` が作るファイル | 骨組みの形                                                                  |
| ログ                  | ログの行の形・メトリクスの項目・SQLite の表の形（[logging.md](logging.md)） |

- SQLite のログの表を変えるときは、表の版（`PRAGMA user_version`）を上げます。

## 約束の範囲に入らないもの

- `#[doc(hidden)]` を付けたもの。
- マクロが使う内部の口（`bengara::kernel` など、文書に書いていないもの）。
- ファジング（壊れた入力を大量に試す検査）のときだけあるもの。
- エラーの文言。文は直すことがあります。文の中身で分岐しないでください。

## prelude

- 0.2 まで、`bengara::prelude` の中身は増やしも減らしもしません。

## 動く Rust の版

- **Rust 1.88** 以上で動きます（MSRV。動くことを約束する最も古い版）。
- 上げるのは minor を上げるときだけです。上げたら CHANGELOG に書きます。

## 修正を出す版

- 最新の minor だけです（0.1.x）。
- 脆弱性の報告は [SECURITY.md](../SECURITY.md) を見てください。

## 非推奨

- 消す前に、できる限り 1 つの minor の間は `#[deprecated]` を付けて残します。
- 付けたものは CHANGELOG の「非推奨」に書きます。

## 関連

- [CHANGELOG.md](../CHANGELOG.md)
- [SECURITY.md](../SECURITY.md)
- [decisions.md](decisions.md)
