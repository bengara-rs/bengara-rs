# 入力の検査（バリデーション）

送られてきた値が、決めた形になっているかを確かめます。Laravel の Validation に当たります。

## 入力を読む

| メソッド                           | 読むところ                   |
|------------------------------------|------------------------------|
| `req.input("title")`               | 本文とクエリ。**本文が優先** |
| `req.input_all()`                  | 同上。全部                   |
| `req.form("title")` / `form_all()` | フォームの本文だけ           |
| `req.query("q")` / `query_all()`   | クエリだけ                   |
| `req.json::<T>()`                  | 本文を型に読む               |

- 本文を読むのは、`Content-Type` がフォーム（`application/x-www-form-urlencoded`）か JSON のときです。
- JSON は最上位の値だけを読みます。数値や真偽値は、見たままの文字になります。入れ子は `req.json::<T>()` で読みます。
- 同じ名前が何個も届いたときは、最初の 1 個を使います。

## 検査する

```rust
pub async fn store(req: Request) -> Result<Response> {
    let input = req.validate(&[
        ("title", "required|max:255"),
        ("email", "required|email"),
        ("age",   "nullable|integer|between:0,150"),
    ])?;

    let title = input.get("title");
    let age: i64 = input.get_as("age").unwrap_or(0);
    // ...
}
```

落ちたら `?` で返すだけです。画面の相手は**前の画面に戻り**、API の相手には **422** を返します
（下の「落ちたとき」）。

### `rules!` で書くとコンパイルのときに確かめる

```rust
let input = req.validate(bengara::rules! {
    "title" => "required|max:255",
    "age"   => "nullable|integer|between:0,150",
})?;
```

- 中身は `&[("title", "required|max:255"), ..]` と同じです。
- 規則の書き間違い（下の「規則の書き間違いは 500 になる」）を、**コンパイルで止めます**。
- 止まるとき、エラーの文は `検査の規則: ` で始まります。
- 項目名と規則は、文字列をそのまま書きます（変数は渡せません）。

## 型に読む（FormRequest）

規則と読み先の型を 1 つにまとめます。Laravel の FormRequest に当たります。
置き場所は `app/Http/Requests/` です。ほかと同じく、大文字始まりのファイルに同じ名前の型を書きます。

```rust
// app/Http/Requests/StorePost.rs
use bengara::FormRequest;

#[derive(bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
pub struct StorePost {
    pub title: String,
    pub age: Option<i64>,
    pub published_at: Option<bengara::Carbon>,
}

impl FormRequest for StorePost {
    const RULES: &'static [(&'static str, &'static str)] = bengara::rules! {
        "title"        => "required|max:255",
        "age"          => "nullable|integer",
        "published_at" => "nullable|date",
    };
}
```

```rust
// app/Http/Controllers/PostController.rs
use bengara::prelude::*;
use crate::app::http::requests::StorePost;

pub struct PostController;

impl PostController {
    pub async fn store(req: Request) -> Result<Response> {
        let input: StorePost = req.validate_as()?;
        // input.title, input.age ...
    }
}
```

| 起きたこと             | 結果                                      |
|------------------------|-------------------------------------------|
| 検査に落ちた           | `validate` と同じ（前の画面に戻すか 422） |
| 通った値を型に読めない | 500（規則と型が食い違っている）           |

- `authorize()` はありません。誰に許すかは [authorization.md](authorization.md) の書き方で確かめます。
- 通った値だけを読みます。規則に書いていない項目は、送られてきても読みません。

### 読める型

| 項目の型                                           | 読み方                                                                      | 読めないとき |
|----------------------------------------------------|-----------------------------------------------------------------------------|--------------|
| `String`                                           | そのまま                                                                    | —            |
| 整数（`i8`〜`i64`、`u8`〜`u64`、`isize`、`usize`） | 前後の空白を落として読む                                                    | 500          |
| `f32` / `f64`                                      | 同上                                                                        | 500          |
| `bool`                                             | `1` `true` `on` `yes` は真、`0` `false` `off` `no` は偽。大文字小文字は区別 | 500          |
| `Option<T>`                                        | 空文字と、無い項目は `None`。それ以外は `T` として読む                      | —            |
| `bengara::Carbon`                                  | `Carbon::parse` で読む                                                      | 500          |
| 中身の無い列挙（`enum Role { Admin, User }`）      | 値を変種の名前として読む。`#[serde(rename_all = "lowercase")]` などが効く   | 500          |
| それ以外（`Vec`・入れ子の構造体など）              | 読まない                                                                    | 500          |

- `nullable` の項目は `Option` にします。空のとき空文字が入り、`Option` でなければ読めません。
- `Option` でない項目が規則に無いと、500 になります（「値がありません」）。
- 500 の文には項目名が出ます。
- `validate` の結果からも読めます。`req.validate(..)?.to_typed::<T>()?` です。

### `RULES` は `rules!` で書く

| 書き方                          | `cargo build`（`serve`・`test` を含む） | `cargo check` |
|---------------------------------|-----------------------------------------|---------------|
| `RULES` を `rules!` で書く      | 止まる                                  | **止まる**    |
| `RULES` を `&[(.., ..)]` で書く | `validate_as` を使う所で止まる          | 止まらない    |

## 通った値

| メソッド             | 返すもの                             |
|----------------------|--------------------------------------|
| `get(field)`         | 値。無ければ空文字                   |
| `try_get(field)`     | 値。検査した項目に無ければ `None`    |
| `get_as::<T>(field)` | 型を変えた値。変えられなければエラー |
| `all()`              | 全部の組                             |
| `has(field)`         | 検査した項目に入っているか           |

**入るのは、規則を書いた項目だけです。**

| 送ったもの       | 規則              | 入るもの     |
|------------------|-------------------|--------------|
| 何も送らない     | `max:10`          | **入らない** |
| 何も送らない     | `nullable`        | 空文字       |
| `age=""`         | `nullable`        | 空文字       |
| `password="   "` | `nullable\|min:8` | 空文字       |

`nullable` の項目は、送られてこなくても `has` が真になります。「送られてきたか」は分かりません。

### 前後の空白は落とす

値の前後の空白を落としてから検査します。Laravel の `TrimStrings` と同じです。

- `" 5 "` は `5` として検査し、`5` が入ります。間の空白は残ります。
- `req.input()` で直に読むと、送られてきたままです。
- `current_password` `password` `password_confirmation` の 3 つだけは落としません。
  そのため `password` の `min:8` は、前後の空白も 1 文字に数えます。
- **落とさないのは、この 3 つの名前だけです。** `new_password` などは前後の空白が落ちます。
  空白で始まるパスワードが、登録とログインで別の値になります。パスワードの項目名はこの 3 つにそろえてください。

## 使える規則

規則は `|` でつなぎます。

| 分類       | 規則                                               |
|------------|----------------------------------------------------|
| 必須と省略 | `required`、`nullable`                             |
| 型         | `integer`（`int`）、`numeric`、`boolean`（`bool`） |
| 形         | `email`、`url`、`alpha`、`alpha_num`、`alpha_dash` |
| 大きさ     | `min:n`、`max:n`、`between:a,b`、`size:n`          |
| 値         | `in:a,b,c`、`starts_with:x`、`ends_with:x`         |
| 日付       | `date`                                             |
| 項目どうし | `confirmed`、`same:other`、`different:other`       |
| ファイル   | `file`、`image`、`mimes:jpg,png`                   |

| 規則         | 通るもの                                                                  |
|--------------|---------------------------------------------------------------------------|
| `boolean`    | `1` `0` `true` `false` `on` `off` `yes` `no`                              |
| `numeric`    | 数として読める値。`inf` `NaN` `1e400` は通さない                          |
| `email`      | `@` が 1 つあり、後ろに `.` がある。空白を含まない（詳しくは下）          |
| `url`        | `http://` か `https://` で始まり、直後にホストがある。空白を含まない      |
| `alpha` など | 「文字」かどうかを見る。**ひらがな・漢字も通る**（Laravel と同じ）        |
| `confirmed`  | `項目名_confirmation` と同じか（`password` なら `password_confirmation`） |
| `date`       | `YYYY-MM-DD` か `YYYY-MM-DD HH:MM(:SS)`（`Carbon::parse` が読める形）     |

- `boolean` は Laravel より広く、`on` `off` `yes` `no` も通します（Laravel は `1` `0` と JSON の `true` `false` だけ。文字列の `"true"` も落とす）。
  大文字小文字は区別します（`TRUE` は落ちる）。
- `email` は形だけを見ます（届くかは見ません）。次のものは落とします。
  - `@` の前に `,` `;` `<` `>` `(` `)` `[` `]` `\` `"` がある。`.` で始まる・終わる・続く（`a..b@x.com`）
  - `@` の後ろの `.` で区切った部分が空（`a@b..cc`）・`-` で始まるか終わる（`a@-x.com`）・
    英数字と `-` 以外の ASCII を含む（`a@x_y.com`）
- 日本語を含むアドレス（`ユーザー@例え.jp`）は通します。
- `date` の秒は省けます（`2026-11-20 09:30` は 09:30:00）。
- `date` は Laravel より狭く、`2026/11/20`・`2026-1-5`・`+09:00` などの時差付きは落とします。
- 区切りの `T`・末尾の `Z`・小数の秒は通ります。
- 英数字だけに限りたいときは、自分で確かめます。

### min / max / between / size の数え方

**`integer` か `numeric` が付いているときだけ、値の大小で比べます。** 付いていなければ文字数です。

| 書き方                            | 送った値        | 結果                 |
|-----------------------------------|-----------------|----------------------|
| `("age", "integer\|min:18")`      | `age=20`        | 通る（値が 18 以上） |
| `("password", "required\|min:8")` | `password=9`    | **落ちる**（1 文字） |
| `("title", "max:50")`             | `title=9999999` | **通る**（7 文字）   |

- 文字数はバイト数ではありません。`あいう` は 3 文字です。
- 数として読めない値には、大きさの規則は何も言いません。理由は「整数で入力してください」などの 1 件だけです。
- ファイルが送られた項目は、大きさを **KB（1024 バイト）** で比べます。下の「ファイルを検査する」を見てください。

### ファイルを検査する

`multipart/form-data` で送られたファイルを検査します。受け取り方は
[requests-and-responses.md](requests-and-responses.md) の「ファイルを受け取る」にあります。

```rust
req.validate(rules! {
    "title"  => "required|max:255",
    "avatar" => "required|image|mimes:jpg,png|max:2048",
})?;
let avatar = req.file("avatar"); // 通ったファイルは file() で読む
```

| 規則            | 通るもの                                                           |
|-----------------|--------------------------------------------------------------------|
| `image`         | 中身が JPEG・PNG・GIF・WebP・BMP。SVG は通さない（Laravel と同じ） |
| `mimes:jpg,png` | 中身から推した種類が、並べたどれか。`jpg` と `jpeg` は同じ         |
| `max:2048`      | 2048 KB 以下（ファイルのときは KB。Laravel と同じ）                |
| `min:10`        | 10 KB 以上                                                         |
| `between:a,b`   | a KB 以上 b KB 以下                                                |
| `size:n`        | ちょうど n KB                                                      |

- **送られてきた名前と MIME は見ません。** 中身の先頭のバイトで決めます。`a.png` と名乗る HTML は `image` で落ちます。
- `mimes` に書けるのは `jpg` `jpeg` `png` `gif` `webp` `bmp` だけです。ほかの種類（`pdf` など）は
  中身から推せないので、書くと規則の書き間違いになります（`rules!` ならコンパイルで止まります）。
- ファイルが送られれば `required` は通ります。送られず、文字も無ければ「必ず入力してください」で落ちます。
- ファイルではなく文字が送られたら、`file`・`image`・`mimes` は落ちます。
- 同じ名前のファイルが何個かあるとき（`photos[]`）は、`("photos", "image")` が全部に当たります。
  1 つでも落ちれば、理由は 1 件です。
- 通ったファイルは、戻り値（`Validated`）にも型（`validate_as`）にも入りません。`req.file()` で読みます。
- 検査に落ちたとき、ファイルは前の入力（`old`）に入りません。もう一度選んでもらいます（Laravel と同じ）。
- `image:allow_svg` の引数は読みません。SVG はいつも通しません。

### 空文字も検査する

検査を飛ばすのは、次の 2 つのときだけです。

- 項目が送られてこなかった
- 空で、`nullable` が付いている

**空文字（`""`）が送られてきたら検査します。** `("role", "in:admin,user")` に `role=""` を送ると落ちます。
空でよい項目には `nullable` を付けます。

```rust
("role", "nullable|in:admin,user")
```

`required` の項目は、空文字でも空白だけでも「必ず入力してください」で落ちます。

## 落ちたとき

相手によって返すものが変わります。Laravel と同じ流れです。

| 相手                                                     | 返すもの                                 |
|----------------------------------------------------------|------------------------------------------|
| JSON を求めている（`Accept: application/json` など）     | 422 と理由の JSON（下の形）              |
| セッションの Cookie を持ち、`Referer` から戻り先が決まる | 302 で前の画面へ。理由と入力を覚えておく |
| それ以外（Cookie が無い・`Referer` が無いなど）          | 422 と、理由を並べた画面                 |

### 前の画面に戻す条件

次をすべて満たすときだけ戻します。どれかが欠けたら 422 です。

| 条件                                    | 理由                                              |
|-----------------------------------------|---------------------------------------------------|
| JSON を求めていない                     | API の相手には 422 の JSON を返す                 |
| セッションが置き場所から読めた          | Cookie の無い相手にセッションのファイルを作らない |
| 戻り先が安全に決まる（下の表）          | 外のサイトへ飛ばすリダイレクトを作らない          |
| GET・HEAD なら、戻り先が今の URL でない | 自分へ戻り続けるのを防ぐ                          |
| 理由の合計が 64 KiB 以下                | 大きな値をセッションに書かない                    |

戻り先の決め方です。

| 項目     | 決めたこと                                                                                      |
|----------|-------------------------------------------------------------------------------------------------|
| 元       | `Referer` ヘッダーだけ                                                                          |
| 一致     | `Referer` のスキーム・ホスト・ポートが `APP_URL` と同じか、ホストとポートが要求の `Host` と同じ |
| 入れる形 | `Location` にはパスとクエリだけを入れる                                                         |
| 断る形   | パスが `//` で始まる・`\` を含む・制御文字や空白・ASCII でない文字を含む                        |

- 開発中に `APP_URL=http://localhost:8000` のまま `127.0.0.1:8000` から開いても戻れるよう、`Host` との一致も見ます。
- 戻った画面では、テンプレートの `errors` で理由を、`old()` で前の入力を出せます（[views.md](views.md)）。
  どちらも次のリクエストで 1 回だけ読めます。
- `with_exceptions` で `Error::Validation` の見せ方を差し替えたときは、そちらが先です（戻しません）。
- Laravel の `url()->previous()` と違い、`Referer` が無いときにセッションの前の URL は使いません。

### JSON の形

JSON を求める相手には、次の形で返します。

```json
{
  "message": "入力に誤りがあります。",
  "errors": {
    "email": ["email はメールアドレスの形で入力してください。"],
    "title": ["title は必ず入力してください。"]
  }
}
```

- 1 つの項目に理由が何件も付くことがあります。項目は名前の順に並びます。
- 戻さない画面の相手には、同じ理由を並べた 422 の画面を返します。JSON を求めているかの決め方は [requests-and-responses.md](requests-and-responses.md) にあります。
- **メッセージは日本語で固定です。** 多言語の仕組み（`resources/lang/`）とはつながっていません。
  言い回しを変えたいときは、下の「自分でエラーを組み立てる」を使います。

### 前の入力を覚えている

セッションを使っていれば、落ちた入力を次のリクエストで読めます。

```rust
req.session().old("title")   // 落ちたときに送られてきた title
```

前の画面に戻さず 422 を返すときも、Cookie を持つ相手なら `validate` は入力を覚えます（今までどおり）。

次のときは覚えません。

| 場合                               | 理由                                         |
|------------------------------------|----------------------------------------------|
| セッションの Cookie を持たない相手 | 壊れた入力だけでセッションを増やされないため |
| 入力の合計が 64 KiB を超える       | 大きな入力をセッションに書かないため         |
| 秘密になりうる名前の項目           | 下の「覚えない項目」                         |

## 自分でエラーを組み立てる

DB を見る確認（重複など）は、自分で書いて足します。

```rust
let mut errors = ValidationErrors::default();
errors.add("title", "すでに同じ題名があります。");
if !errors.is_empty() {
    return Err(Error::Validation(Box::new(errors)));
}
```

- 返したあとは `validate` と同じです。条件がそろえば前の画面に戻し、理由と入力を覚えます。
  `validate` を通っていなくても、送られてきた入力（「覚えない項目」を除く）を覚えます。
- ハンドラだけでなく、`StartSession` より内側のミドルウェアで返しても同じです。

| メソッド     | 返すもの               |
|--------------|------------------------|
| `has(field)` | その項目に理由があるか |
| `get(field)` | その項目の理由の一覧   |
| `first()`    | 最初の理由             |
| `all()`      | 全部                   |
| `len()`      | 項目の数               |
| `total()`    | 理由の総数             |

## 規則の書き間違いは 500 になる

**書き間違えた規則は、黙って通しません。** 知らない規則名なら、使える名前の一覧がメッセージに出ます。

| 書き間違い           | 理由                   |
|----------------------|------------------------|
| `maxx:10`            | 知らない規則名         |
| `min`                | 引数を忘れた           |
| `max:nan` `min:inf`  | 数として使えない       |
| `size:-3` `size:1.5` | `size` は 0 以上の整数 |
| `between:10,1`       | 下限が上限より大きい   |
| `in:` `in:,,`        | 許す値が 1 つも無い    |
| `mimes:` `mimes:pdf` | 中身から推せない種類   |

- `in:admin,user,` のように空の値があっても、`role=`（空）は通りません。
- `rules!` で書くと、ほとんどは**コンパイルで止まります**。`between:10,1` と `max:1e400` のような桁あふれだけは、実行時に
  500 です。

## 覚えない項目

次の語を名前に含む項目は、`old()` で覚えません。セッションのファイルに平文で残さないためです。
ログで `***` に伏せる項目も同じです。

```
password  passwd  pwd  pass  passphrase  secret  token  key  apikey
credential  credentials  cvv  cvc  csc  card  ssn  otp  totp  pin
code  seed  mnemonic  recovery  backup  answer  signature  ticket
```

- 名前を `_` `-` `.` などと大文字の境目で語に切って比べます。語の末尾の数字は無視します。

| 名前                                    | 覚えるか |
|-----------------------------------------|----------|
| `new_password_confirmation` `password1` | 覚えない |
| `apiKey`（`api` と `Key` に切れる）     | 覚えない |
| `zip_code` `country_code`               | 覚えない |
| `shipping_address` `keyword`            | 覚える   |

`zip_code` も覚えないので、落ちたフォームを出し直すと郵便番号の欄は空に戻ります。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [session.md](session.md)
