# 版と互換性

bengara の版の付け方と、版を上げても壊さないと約束する範囲です。Laravel の「Release Notes」に当たります。

- 変更の中身は [CHANGELOG.md](../CHANGELOG.md) にあります。
- 公開した版はまだありません。最初の版は 0.1.0 にします。

## 版の付け方

| 時期         | 壊す変更        | 壊さない変更 |
|--------------|-----------------|--------------|
| 0.x の間     | minor（0.1 → 0.2） | patch（0.1.0 → 0.1.1） |
| 1.0 から先   | major           | minor か patch（SemVer どおり） |

- 「壊す変更」は、下の「壊さない約束の範囲」を変えるものです。
- Cargo は `0.1` と書いた依存を 0.1.x の中だけで上げます。だから 0.x の間も、`bengara = "0.1"` と書いておけば壊れる版には上がりません。

## 壊さない約束の範囲（公開 API）

| 範囲                   | 例                                                                      |
|------------------------|-------------------------------------------------------------------------|
| 公開の項目             | クレート直下（`bengara::…`）と `bengara::prelude` から使える型・関数・マクロ |
| 文書に書いた動き       | この `docs/` に書いた動き                                               |
| `.env` の名前と意味    | `APP_KEY`・`DB_CONNECTION`・`LOG_CHANNEL` など                          |
| 機能フラグの名前       | `sqlite`・`websocket` など                                              |
| コマンド               | `cargo artisan …` の名前と引数                                          |
| `init` が作るファイル  | 骨組みの形                                                              |
| ログ                   | ログの行の形と、SQLite の表の形（[logging.md](logging.md)）             |

## 約束の範囲に入らないもの

- `#[doc(hidden)]` を付けたもの。
- マクロのための内部の口（`bengara::kernel` など、文書に書いていないもの）。
- `cfg(fuzzing)` のときだけあるもの（ファジングのための口）。
- エラーの文言。文は直すことがあります。文の中身で分岐しないでください。

## prelude

- 0.2 まで、`bengara::prelude` の中身は増やしも減らしもしません（[判断 78](decisions.md)）。

## 動く Rust の版（MSRV）

- いまは **Rust 1.88** 以上です。
- 上げるのは minor を上げるときだけです。上げたら CHANGELOG に書きます。

## 修正を出す版

- 最新の minor だけです（いまは 0.1.x）。
- 脆弱性の報告は [SECURITY.md](../SECURITY.md) を見てください。

## 非推奨

- 消す前に、できる限り 1 つの minor の間は `#[deprecated]` を付けて残します。
- 付けたものは CHANGELOG の「非推奨」に書きます。

## ログの形とメトリクスの項目

- ログの行の形・メトリクスの項目・SQLite の表を変えるときは、壊す変更として扱います。
- SQLite の表を変えるときは `PRAGMA user_version` を上げます（[logging.md](logging.md)）。

## 関連

- [CHANGELOG.md](../CHANGELOG.md)
- [SECURITY.md](../SECURITY.md)
- [decisions.md](decisions.md)
