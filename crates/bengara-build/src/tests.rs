//! `bengara-build` のテスト。`lib.rs` から分けて置いています（中身は `lib.rs` の非公開の関数を直接呼びます）。

use super::*;

/// テスト用の作業場所。テストごとに別の名前にする。
fn temp_root(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "bengara_build_{tag}_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn touch(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn stack_of(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// 生成コードから、`head` で始まる一覧の定数（`];` の行の手前まで）を取り出す。
fn list_block<'a>(code: &'a str, head: &str) -> Vec<&'a str> {
    code.lines()
        .skip_while(|line| !line.starts_with(head))
        .take_while(|line| *line != "];")
        .collect()
}

/// ディレクトリへのリンクを作る。作れない環境では `false`。
///
/// Windows は権限（開発者モード）が無いと作れません。そのときはテストを飛ばします。
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link).is_ok();
    #[cfg(windows)]
    return std::os::windows::fs::symlink_dir(target, link).is_ok();
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

#[test]
fn publicの中のディレクトリへのリンクは埋め込まない() {
    // 以前はファイルとして `include_bytes!` に入り、リリースビルドだけが落ちた
    // （`public/storage` → `storage/app/public` のリンクが典型）。
    let root = temp_root("public_link");
    let public = root.join("public");
    touch(&public.join("robots.txt"), "User-agent: *\n");
    touch(&public.join("css/app.css"), "body {}\n");
    touch(&root.join("storage/app/public/a.txt"), "a");
    if !link_dir(&root.join("storage/app/public"), &public.join("storage")) {
        let _ = fs::remove_dir_all(&root);
        return;
    }

    let mut generator = Generator::default();
    let mut total = 0;
    generator.collect_public(&public, "", &mut total, 0);

    let keys: Vec<&str> = generator.public.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["css/app.css", "robots.txt"]);
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    // リンクの先の中身は数えない。
    assert_eq!(total, 14 + 8);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn publicの中のふつうのファイルは今までどおり埋め込む() {
    let root = temp_root("public_plain");
    let public = root.join("public");
    touch(&public.join("robots.txt"), "User-agent: *\n");
    touch(&public.join("img/logo.svg"), "<svg/>");
    touch(&public.join(".hidden"), "x");

    let mut generator = Generator::default();
    let mut total = 0;
    generator.collect_public(&public, "", &mut total, 0);

    let keys: Vec<&str> = generator.public.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["img/logo.svg", "robots.txt"]);
    assert_eq!(total, 6 + 14);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn viewsはhtmlだけを鍵の昇順で埋め込む() {
    let root = temp_root("views_plain");
    let views = root.join("resources/views");
    touch(&views.join("welcome.html"), "<p>hi</p>");
    touch(&views.join("posts/index.html"), "{{ title }}");
    touch(&views.join("posts/notes.txt"), "x");
    touch(&views.join(".hidden.html"), "x");

    let mut generator = Generator::default();
    generator.collect_views(&views, "", 0);

    let keys: Vec<&str> = generator.views.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["posts/index.html", "welcome.html"]);
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);

    let code = generator.views_code();
    assert!(code.starts_with(
        "#[doc(hidden)]\nstatic __BENGARA_VIEWS: &[(&str, &str)] = &[(\"posts/index.html\", include_str!("
    ));
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn viewsの中身がutf8でなければ知らせる() {
    let root = temp_root("views_utf8");
    let views = root.join("resources/views");
    touch(&views.join("ok.html"), "ok");
    fs::write(views.join("bad.html"), [0xff, 0xfe, 0x00]).unwrap();

    let mut generator = Generator::default();
    generator.collect_views(&views, "", 0);

    let keys: Vec<&str> = generator.views.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["ok.html"]);
    assert_eq!(
        generator.errors,
        ["resources/views/bad.html の中身が UTF-8 ではありません"]
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn viewsの中のディレクトリへのリンクは埋め込まない() {
    let root = temp_root("views_link");
    let views = root.join("resources/views");
    touch(&views.join("a.html"), "a");
    touch(&root.join("other/b.html"), "b");
    if !link_dir(&root.join("other"), &views.join("other")) {
        let _ = fs::remove_dir_all(&root);
        return;
    }

    let mut generator = Generator::default();
    generator.collect_views(&views, "", 0);

    let keys: Vec<&str> = generator.views.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["a.html"]);
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 埋め込むviewsが無くても空の一覧をhooksに渡す() {
    let code = Generator::default().finish();
    assert!(code.contains("static __BENGARA_VIEWS: &[(&str, &str)] = &[];"));
    assert!(code.contains("views: || __BENGARA_VIEWS,"));
}

#[test]
fn 大文字始まりはスネークケースになる() {
    assert_eq!(to_module_name("HomeController").unwrap(), "home_controller");
    assert_eq!(to_module_name("Http").unwrap(), "http");
    assert_eq!(to_module_name("EnsureAdmin").unwrap(), "ensure_admin");
}

#[test]
fn 小文字はそのまま() {
    assert_eq!(to_module_name("web").unwrap(), "web");
    assert_eq!(to_module_name("app").unwrap(), "app");
}

#[test]
fn 日付で始まる名前にはmが付く() {
    assert_eq!(
        to_module_name("2026_09_29_000000_create_users_table").unwrap(),
        "m2026_09_29_000000_create_users_table"
    );
}

#[test]
fn ハイフンはアンダースコアになる() {
    assert_eq!(to_module_name("my-file").unwrap(), "my_file");
}

#[test]
fn 連続する大文字は1文字ずつ区切る() {
    assert_eq!(to_module_name("API").unwrap(), "a_p_i");
}

#[test]
fn 置き場所はモジュール名の並びで見る() {
    // 置き場所そのもの。
    assert!(is_exactly(
        &stack_of(&["app", "console", "commands"]),
        COMMANDS_DIR
    ));
    // 下にあるものも一覧に入れる。
    assert!(under(&stack_of(&["app", "jobs", "mail"]), JOBS_DIR));
    assert!(!is_exactly(&stack_of(&["app", "jobs", "mail"]), JOBS_DIR));
    // 別の場所は入らない。
    assert!(!under(&stack_of(&["app", "foo", "commands"]), COMMANDS_DIR));
    assert!(!under(&stack_of(&["tests", "migrations"]), MIGRATIONS_DIR));
    assert!(!under(&stack_of(&["database"]), MIGRATIONS_DIR));
}

#[test]
fn 大文字だけのディレクトリ名は置き場所にならない() {
    // `app/JOBS/` のモジュール名は `j_o_b_s`。生のディレクトリ名で判定すると
    // ここが真になり、`crate::app::jobs::JOBS` を参照する壊れたコードができる。
    assert_eq!(to_module_name("JOBS").unwrap(), "j_o_b_s");
    assert!(!under(&stack_of(&["app", "j_o_b_s"]), JOBS_DIR));
    // `app/Jobs/` は `jobs` になるので一致する。
    assert_eq!(to_module_name("Jobs").unwrap(), "jobs");
    assert!(under(&stack_of(&["app", "jobs"]), JOBS_DIR));
}

#[test]
fn 型の名前に使える名前だけを見分ける() {
    assert!(is_ident("HomeController"));
    assert!(is_ident("User2"));
    assert!(!is_ident("My-Model"));
    assert!(!is_ident("My Model"));
    assert!(!is_ident("2Model"));
    assert!(!is_ident("type"));
    assert!(!is_ident(""));
}

#[test]
fn 言語の表は言語名の昇順になる() {
    let mut generator = Generator::default();
    generator.lang.push((
        "ja".to_string(),
        vec![("m.a".to_string(), "値".to_string())],
    ));
    generator.lang.push(("en".to_string(), Vec::new()));
    let code = generator.lang_code();
    let table = code.lines().last().unwrap().to_string();
    assert!(table.find("\"en\"").unwrap() < table.find("\"ja\"").unwrap());
}

#[test]
fn 鍵は昇順に並び重複は知らせる() {
    let root = temp_root("lang");
    let dir = root.join("resources/lang");
    touch(&dir.join("ja.toml"), "[m]\nb = \"2\"\na = \"1\"\n");
    touch(&dir.join("en.toml"), "[m]\na = \"1\"\na = \"2\"\n");

    let mut generator = Generator::default();
    generator.read_lang(&root);

    let ja = generator
        .lang
        .iter()
        .find(|(locale, _)| locale == "ja")
        .unwrap();
    let keys: Vec<&str> = ja.1.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["m.a", "m.b"]);
    assert!(generator
        .errors
        .iter()
        .any(|e| e.contains("m.a") && e.contains("2回")));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 言語は入れ子のディレクトリも読む() {
    let root = temp_root("lang_dir");
    let dir = root.join("resources/lang");
    // Laravel 本来の置き方。鍵の頭にファイル名が付く。
    touch(&dir.join("ja/messages.toml"), "[x]\ny = \"値\"\n");
    touch(&dir.join("ja/validation.toml"), "required = \"必須です\"\n");
    // 直下の ja.toml もこれまでどおり読める。1つの表にまとまる。
    touch(&dir.join("ja.toml"), "[top]\nz = \"直下\"\n");

    let mut generator = Generator::default();
    generator.read_lang(&root);

    assert_eq!(generator.lang.len(), 1, "言語は ja だけ");
    let keys: Vec<&str> = generator.lang[0]
        .1
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["messages.x.y", "top.z", "validation.required"],
        "鍵の昇順で並ぶ"
    );
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 入れ子のシーダーとマイグレーションも一覧に入る() {
    let root = temp_root("nested");
    touch(&root.join("database/seeders/UserSeeder.rs"), "");
    touch(&root.join("database/seeders/Demo/AdminSeeder.rs"), "");
    touch(&root.join("database/migrations/2026_01_01_000000_a.rs"), "");
    touch(
        &root.join("database/migrations/2027/2027_01_01_000000_b.rs"),
        "",
    );

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();

    // 一覧は1つだけ。入れ子で集めたものが親に持ち上がっている。
    assert_eq!(code.matches("pub const SEEDERS").count(), 1);
    assert_eq!(code.matches("pub const MIGRATIONS").count(), 1);

    let seeders = list_block(&code, "pub const SEEDERS").join("\n");
    // 名前順に並ぶ。
    assert!(
        seeders.find("\"AdminSeeder\"").unwrap() < seeders.find("\"UserSeeder\"").unwrap(),
        "{seeders}"
    );
    assert!(seeders.contains("crate::__bengara_database_seeders_demo_admin_seeder::run()"));

    let migrations = list_block(&code, "pub const MIGRATIONS").join("\n");
    assert!(migrations.contains("\"2026_01_01_000000_a\""));
    assert!(migrations.contains("\"2027_01_01_000000_b\""));

    // 空の一覧ではなく、生成した定数を参照している。
    assert!(code.contains("seeders: || crate::database::seeders::SEEDERS"));
    assert!(code.contains("migrations: || crate::database::migrations::MIGRATIONS"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ジョブは引数の型を推論する口から呼ぶ() {
    let root = temp_root("jobs");
    touch(&root.join("app/Jobs/SendWelcome.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();

    let jobs = list_block(&code, "pub const JOBS");
    assert_eq!(jobs[0], "pub const JOBS: &[::bengara::Job] = &[");
    assert!(
        jobs[1].starts_with(
            "    ::bengara::Job { name: \"SendWelcome\", \
             handle: |__payload| ::bengara::queue::__call_job(\
             crate::__bengara_app_jobs_send_welcome::handle, __payload) }, // bengara: "
        ),
        "{jobs:?}"
    );
    assert!(code.contains("jobs: || crate::app::jobs::JOBS"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 入れ子で一覧の名前がぶつかったら知らせる() {
    let root = temp_root("dup_seeder");
    touch(&root.join("database/seeders/UserSeeder.rs"), "");
    touch(&root.join("database/seeders/Demo/UserSeeder.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(
        generator
            .errors
            .iter()
            .any(|e| e.contains("シーダー") && e.contains("UserSeeder")),
        "{:?}",
        generator.errors
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 控えのファイルはビルドを止めない() {
    // 以前は `User.old` が大文字始まりなので型の再エクスポートの対象になり、
    // 識別子にできないので `compile_error!` になっていた。置いただけで
    // `cargo build` が全部止まっていた。
    let root = temp_root("backup");
    touch(&root.join("app/Models/User.rs"), "pub struct User;");
    touch(&root.join("app/Models/User.old.rs"), "pub struct User;");
    touch(&root.join("app/Models/Copy of User.rs"), "pub struct User;");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    // 本物だけを取り込む。控えは取り込まない。
    assert!(code.contains("User.rs"));
    assert!(!code.contains("User.old.rs"));
    assert!(!code.contains("Copy of User.rs"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn マイグレーションの控えはビルドを止める() {
    // ほかの置き場所では警告だけだが、マイグレーションは黙って一覧から
    // 落ちると `migrate` がその表を作らないまま成功を報告する。
    let root = temp_root("migration-backup");
    touch(&root.join("database/migrations/2026_01_01_000000_a.rs"), "");
    touch(
        &root.join("database/migrations/2026_01_02_000000_b.v2.rs"),
        "",
    );

    let mut generator = Generator::default();
    generator.run(&root);

    assert_eq!(generator.errors.len(), 1, "{:?}", generator.errors);
    assert!(generator.errors[0].contains("2026_01_02_000000_b.v2.rs"));
    assert!(generator.errors[0].contains("マイグレーション"));
    let code = generator.finish();
    assert!(code.contains("compile_error!"));
    // 本物のほうは今までどおり一覧に入る。
    assert!(code.contains("\"2026_01_01_000000_a\""));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 日付で始まらないマイグレーションはビルドを止める() {
    // 警告だけだったころは、`migrate` が表を作らないまま成功を報告し、
    // あとのテストが「テーブルが無い」という遠い場所のエラーで落ちていた。
    let root = temp_root("migration-nodate");
    touch(&root.join("database/migrations/CreateUsersTable.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    assert_eq!(generator.errors.len(), 1, "{:?}", generator.errors);
    assert!(generator.errors[0].contains("CreateUsersTable.rs"));
    // 直し方（日付から始める例）を文面に入れる。
    assert!(generator.errors[0].contains("2026_10_05_000000_create_posts_table.rs"));
    assert!(generator.finish().contains("compile_error!"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 控えのディレクトリは取り込まない() {
    // `cp -r` で作った控え。`file_stem` で名前を取っていたころは `old.bak` が
    // `old` になり、中の控えのマイグレーションが `MIGRATIONS` に入って
    // 実際に走っていた。`Controllers copy/` は同じルートを2回登録していた。
    let root = temp_root("backup-dir");
    touch(&root.join("app/Http/Controllers/HomeController.rs"), "");
    touch(
        &root.join("app/Http/Controllers copy/HomeController.rs"),
        "",
    );
    touch(&root.join("database/migrations/2026_01_01_000000_a.rs"), "");
    touch(
        &root.join("database/migrations/old.bak/2025_01_01_000000_z.rs"),
        "",
    );

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    // 本物だけを取り込む。
    assert!(code.contains("HomeController.rs"));
    assert!(!code.contains("Controllers copy"), "{code}");
    assert!(!code.contains("old.bak"), "{code}");
    // 控えのマイグレーションは一覧に入らない。
    assert!(code.contains("\"2026_01_01_000000_a\""));
    assert!(!code.contains("2025_01_01_000000_z"), "{code}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 控えの言語ファイルは読まない() {
    // `cp` で作った控え。`ja.old.toml` を読むと `ja.old` という言語が増え、
    // `ja.old/`（ディレクトリ）を外しているのと扱いが食い違っていた。
    let root = temp_root("backup-lang");
    touch(
        &root.join("resources/lang/ja.toml"),
        "[messages]\nwelcome = \"ようこそ\"\n",
    );
    touch(
        &root.join("resources/lang/ja.old.toml"),
        "[messages]\nwelcome = \"古い\"\n",
    );

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    assert!(code.contains("\"ja\""), "{code}");
    assert!(!code.contains("ja.old"), "{code}");
    assert!(!code.contains("古い"), "{code}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ディレクトリ名は点で切らない() {
    // `file_stem` は `old.bak` を `old` にしてしまう。
    assert_eq!(file_stem(Path::new("a/old.bak")), Some("old"));
    assert_eq!(dir_name(Path::new("a/old.bak")), Some("old.bak"));
    // 名前として使うときは `old_bak` になる（控えとして外す前の形）。
    assert_eq!(to_module_name("old.bak").unwrap(), "old_bak");
}

#[test]
fn 入口の名前のファイルは取り込まずに知らせる() {
    // 以前は `mod.rs` だけ警告し、`main.rs` と `lib.rs` は黙って外していた。
    for stem in ["mod", "main", "lib"] {
        let path = Path::new("app").join("Http").join(format!("{stem}.rs"));
        let message = skipped_file_warning(stem, &path).expect(stem);
        assert!(message.contains(&format!("{stem}.rs")), "{message}");
        assert!(message.contains("取り込みません"), "{message}");
    }
    // 隠しファイルは黙って外す。
    assert_eq!(
        skipped_file_warning(".hidden", Path::new("app/.hidden.rs")),
        None
    );
    assert_eq!(skipped_file_warning("User", Path::new("app/User.rs")), None);
}

#[test]
fn 入口の名前のファイルはモジュールにしない() {
    let root = temp_root("entry-names");
    touch(&root.join("app/Http/main.rs"), "pub fn x() {}\n");
    touch(&root.join("app/Http/lib.rs"), "pub fn y() {}\n");
    touch(&root.join("app/Http/Home.rs"), "pub struct Home;\n");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    assert!(code.contains("Home.rs"), "{code}");
    assert!(!code.contains("main.rs"), "{code}");
    assert!(!code.contains("lib.rs"), "{code}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 控えかどうかは点と空白で見分ける() {
    assert!(is_source_name("User"));
    assert!(is_source_name("home_controller"));
    // `-` はモジュール名にできるので控えとは見ない（型名に使えないのは別の話）。
    assert!(is_source_name("My-Model"));
    assert!(!is_source_name("User.old"));
    assert!(!is_source_name("Copy of User"));
    assert!(!is_source_name("User backup"));
}

#[test]
fn 深すぎるディレクトリは打ち切る() {
    // 上限が無いとスタックを使い切ってパニックする。このクレートは
    // パニックしない決まりなので、警告で知らせて打ち切る。
    let root = temp_root("deep");
    let mut dir = root.join("app");
    for i in 0..(MAX_DEPTH + 3) {
        dir = dir.join(format!("D{i}"));
    }
    touch(&dir.join("Deep.rs"), "pub struct Deep;");
    // 浅いところのファイルは取り込む。
    touch(&root.join("app/Models/User.rs"), "pub struct User;");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    assert!(code.contains("User.rs"));
    assert!(!code.contains("Deep.rs"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 平らな名前は連番も登録する() {
    let mut generator = Generator::default();
    generator.stack.push("app".to_string());

    let first = generator.flat_name("foo_bar", Path::new("app/foo_bar.rs"));
    assert_eq!(first, "__bengara_app_foo_bar");
    // つなげると同じ名前になる2つめは連番になる。
    let second = generator.flat_name("foo_bar", Path::new("app/FooBar.rs"));
    assert_eq!(second, "__bengara_app_foo_bar_2");
    // 連番も登録されているので、素で `foo_bar_2` になるファイルは別の名前になる。
    // 登録していないと、ここで `__bengara_app_foo_bar_2` と二重定義になる。
    let third = generator.flat_name("foo_bar_2", Path::new("app/foo_bar_2.rs"));
    assert_eq!(third, "__bengara_app_foo_bar_2_2");
    assert_eq!(generator.taken_flat.len(), 3);
}

#[test]
fn 中身が同じなら書き直さない() {
    let root = temp_root("write");
    fs::create_dir_all(&root).unwrap();
    let path = root.join(OUT_FILE);

    write_if_changed(&path, "a");
    let first = fs::metadata(&path).unwrap().modified().unwrap();

    // 同じ中身なら触らない（更新時刻が進まない）。
    write_if_changed(&path, "a");
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), first);

    // 違う中身なら書く。
    write_if_changed(&path, "b");
    assert_eq!(fs::read_to_string(&path).unwrap(), "b");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 大きさは小数1桁で出す() {
    // 切り捨てると 8.9 MiB が「8 MiB」になってしまう。
    assert_eq!(mib(8 * 1024 * 1024 + 900 * 1024), "8.9");
    assert_eq!(mib(0), "0.0");
    assert_eq!(mib(1024 * 1024), "1.0");
}

#[test]
fn 無い置き場所は見張らない() {
    let root = temp_root("missing");
    let mut generator = Generator::default();
    generator.run(&root);

    // 無いだけならエラーにしない。生成コードも壊れない。
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    // 1件も `rerun-if-changed` を出さない。無いディレクトリを出すと毎回走り、
    // 代わりにルートを出しても、cargo が中の `target/` までたどるので毎回走る。
    assert!(generator.watched.is_empty(), "{:?}", generator.watched);
    let code = generator.finish();
    assert!(code.contains("migrations: || &[]"));
}

#[test]
fn あるディレクトリだけを見張る() {
    let root = temp_root("watch_some");
    touch(&root.join("app/Models/User.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    // 置いた分は見張る。無いものとルートは出さない。
    assert!(generator.watched.contains(&root.join("app")));
    // 入れ子は出さない。cargo はいちばん上から再帰的にたどる。
    assert!(!generator.watched.contains(&root.join("app/Models")));
    assert!(!generator.watched.contains(&root.to_path_buf()));
    assert!(!generator.watched.contains(&root.join("config")));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn databaseは中を1つずつ見張る() {
    let root = temp_root("watch_database");
    touch(
        &root.join("database/migrations/2026_01_01_000000_create_users.rs"),
        "",
    );
    touch(&root.join("database/seeders/DatabaseSeeder.rs"), "");
    touch(&root.join("database/helpers.rs"), "");
    touch(&root.join("database/database.sqlite"), "");
    touch(&root.join("database/database.sqlite-wal"), "");

    let mut generator = Generator::default();
    generator.run(&root);
    // 置いた名前はどれも正しい。見張り以外の理由で落ちないことを確かめる。
    assert!(generator.errors.is_empty(), "{:?}", generator.errors);

    // SQLite に書くたびに作り直さないよう、database/ そのものは見張らない。
    assert!(!generator.watched.contains(&root.join("database")));
    assert!(!generator
        .watched
        .contains(&root.join("database/database.sqlite")));
    assert!(!generator
        .watched
        .contains(&root.join("database/database.sqlite-wal")));
    // 中のディレクトリと直下の .rs は見張る。
    assert!(generator
        .watched
        .contains(&root.join("database/migrations")));
    assert!(generator.watched.contains(&root.join("database/seeders")));
    assert!(generator
        .watched
        .contains(&root.join("database/helpers.rs")));
    // 無い factories/ は出さない（出すと毎回走る）。
    assert!(!generator.watched.contains(&root.join("database/factories")));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ファイルへのリンクは取り込む() {
    let root = temp_root("file_link");
    touch(&root.join("shared/Post.rs"), "");
    fs::create_dir_all(root.join("app/Models")).unwrap();
    #[cfg(unix)]
    let made =
        std::os::unix::fs::symlink(root.join("shared/Post.rs"), root.join("app/Models/Post.rs"))
            .is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(
        root.join("shared/Post.rs"),
        root.join("app/Models/Post.rs"),
    )
    .is_ok();
    if !made {
        // Windows は権限（開発者モード）が無いと作れない。
        let _ = fs::remove_dir_all(&root);
        return;
    }

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();
    // 普通のファイルと同じく取り込む（モジュール `post` ができる）。
    assert!(code.contains(" as post;"), "{code}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 入れ子で同じコマンド名になったら知らせる() {
    let root = temp_root("dup_command");
    // ファイル名は違うが、どちらもコマンド名は `send-report` になる。
    touch(&root.join("app/Console/Commands/SendReport.rs"), "");
    touch(&root.join("app/Console/Commands/Sub/Send_Report.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(
        generator
            .errors
            .iter()
            .any(|e| e.contains("コマンド") && e.contains("send-report")),
        "{:?}",
        generator.errors
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn コマンド名はファイル名から作る() {
    assert_eq!(to_command_name("SendReport").unwrap(), "send-report");
    assert_eq!(to_command_name("Send_Report").unwrap(), "send-report");
    assert_eq!(to_command_name("Cleanup").unwrap(), "cleanup");
    // 打ちようのない名前になる文字は断る。
    assert!(to_command_name("My Report").is_err());
    assert!(to_command_name("Send.Report").is_err());
    assert!(to_command_name("日本語").is_err());
}

#[test]
fn フレームワークと同じ名前の自作コマンドは知らせる() {
    let path = Path::new("app/Console/Commands/List.rs");
    // ファイル名から作った名前で比べる（`List` → `list`）。
    let name = to_command_name("List").unwrap();
    let message = reserved_command_message(path, &name).expect("知らせる文がありません");
    assert!(message.contains("List.rs"), "{message}");
    assert!(message.contains("`list`"), "{message}");
    for stem in ["Help", "Init", "Serve", "Migrate", "Version", "About"] {
        let name = to_command_name(stem).unwrap();
        assert!(reserved_command_message(path, &name).is_some(), "{stem}");
    }
    // 重ならない名前には何も言わない。
    for stem in ["SendReport", "Listing", "MigrateUsers", "Cleanup"] {
        let name = to_command_name(stem).unwrap();
        assert!(reserved_command_message(path, &name).is_none(), "{stem}");
    }
}

#[test]
fn 同じ名前の自作コマンドも一覧には入れる() {
    // 警告を出すだけで、ビルドは止めない（生成コードの形も変えない）。
    let root = temp_root("reserved_command");
    touch(&root.join("app/Console/Commands/List.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    assert!(code.contains("name: \"list\""), "{code}");

    let _ = fs::remove_dir_all(&root);
}

/// 行の最初の文字列リテラルの中身（`"..."` の間）。
fn first_literal(line: &str) -> Option<&str> {
    let rest = line.split_once('"')?.1;
    Some(rest.split_once('"')?.0)
}

/// 自作コマンドの名前になりうる形か（[`to_command_name`] が作る形）。
///
/// ファイル名は大文字で始まるので、名前は必ず小文字で始まります
/// （`--help` や `-h` とは重なりません）。
fn is_command_shaped(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 本体のソースの `const COMMANDS` の表から、名前を取り出す。
fn names_in_commands_table(source: &str) -> Vec<String> {
    let Some(start) = source.find("const COMMANDS") else {
        return Vec::new();
    };
    source[start..]
        .lines()
        .skip(1)
        .take_while(|line| line.trim() != "];")
        .filter(|line| {
            let line = line.trim();
            line.starts_with("(\"") || line.starts_with('"')
        })
        .filter_map(first_literal)
        // 説明の行（日本語と空白を含む）は、名前の形ではないので落ちる。
        .filter(|name| {
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == ':')
        })
        .map(str::to_string)
        .collect()
}

/// CI の中か。`CI` が空でなく、`false` でも `0` でもないとき。
///
/// GitHub Actions などは `CI=true` を渡します。
fn in_ci(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.and_then(|value| value.to_str())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty() && value != "false" && value != "0")
}

/// 本体の `kernel_impl` のソースを読む。
///
/// `src/kernel_impl.rs` 1 つのときと、`src/kernel_impl/` に分けたとき
/// （`mod.rs` と中の `.rs`）の両方に対応します。分けたときは、中のファイルを
/// 名前順につないで返します。表がどのファイルに移っても突き合わせが続くようにするためです。
fn read_kernel(src: &Path) -> String {
    let mut text = String::new();
    let single = src.join("kernel_impl.rs");
    if single.is_file() {
        text.push_str(
            &fs::read_to_string(&single)
                .unwrap_or_else(|e| panic!("{} を読めません（{e}）", single.display())),
        );
    }
    let dir = src.join("kernel_impl");
    if dir.is_dir() {
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{} を読めません（{e}）", dir.display()))
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_rust_file(path))
            .collect();
        files.sort();
        for file in files {
            text.push_str(
                &fs::read_to_string(&file)
                    .unwrap_or_else(|e| panic!("{} を読めません（{e}）", file.display())),
            );
            text.push('\n');
        }
    }
    assert!(
        !text.is_empty(),
        "{} にも {} にも本体のソースがありません。コマンドの表の置き場所が変わったなら、\
         このテストの読む先を直してください",
        single.display(),
        dir.display()
    );
    text
}

#[test]
fn ciの中かを環境変数から決める() {
    use std::ffi::OsStr;

    for set in ["true", "1", "yes", " true "] {
        assert!(in_ci(Some(OsStr::new(set))), "{set:?}");
    }
    for unset in ["", " ", "false", "0"] {
        assert!(!in_ci(Some(OsStr::new(unset))), "{unset:?}");
    }
    assert!(!in_ci(None));
}

#[test]
fn 本体のソースは1つのファイルでも分けたディレクトリでも読める() {
    // `kernel_impl.rs` を `kernel_impl/` に分けても、突き合わせが止まらないようにする。
    let root = temp_root("read_kernel");
    touch(&root.join("one/kernel_impl.rs"), "names: &[\"serve\"]");
    assert_eq!(
        names_in_kernel_table(&read_kernel(&root.join("one"))),
        ["serve"]
    );

    touch(
        &root.join("split/kernel_impl/mod.rs"),
        "names: &[\"serve\"]",
    );
    touch(
        &root.join("split/kernel_impl/table.rs"),
        "names: &[\"about\", \"list\"]",
    );
    touch(
        &root.join("split/kernel_impl/notes.txt"),
        "names: &[\"nope\"]",
    );
    assert_eq!(
        names_in_kernel_table(&read_kernel(&root.join("split"))),
        ["serve", "about", "list"]
    );

    let _ = fs::remove_dir_all(&root);
}

/// 本体の `kernel_impl.rs` の表（`names: &[..]`）から、名前を取り出す。
fn names_in_kernel_table(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for part in source.split("names: &[").skip(1) {
        let Some((inside, _)) = part.split_once(']') else {
            continue;
        };
        names.extend(inside.split('"').skip(1).step_by(2).map(str::to_string));
    }
    names
}

#[test]
fn フレームワークのコマンド名の写しが本体とそろっている() {
    // このクレートは依存を持てないので、本体のコマンド名を写して持っている。
    // ここで本体のソースと突き合わせて、ずれを見つける。
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bengara");
    if !core.join("Cargo.toml").is_file() {
        // crates.io から取り出した単体では、隣に本体が無い。確かめようがない。
        // ただし CI では飛ばさない。workspace の中で走るので、本体は必ず隣にある。
        // 無いなら置き場所が変わったということで、黙って通ると突き合わせが止まったままになる。
        assert!(
            !in_ci(std::env::var_os("CI").as_deref()),
            "{} に本体のソースがありません。CI では突き合わせを飛ばせません",
            core.display()
        );
        eprintln!("本体のソースが隣に無いので、突き合わせを飛ばします");
        return;
    }
    let read = |relative: &str| {
        let path = core.join(relative);
        fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{} を読めません（{e}）。コマンドの表の置き場所が変わったなら、\
                 このテストの読む先を直してください",
                path.display()
            )
        })
    };

    let kernel = names_in_kernel_table(&read_kernel(&core.join("src")));
    let extra = names_in_commands_table(&read("src/commands.rs"));
    let database = names_in_commands_table(&read("src/database/commands.rs"));
    // 取り出し方が合っているかを、必ずある名前で確かめる。
    // ここで落ちたら、本体の表の書き方が変わっている。
    assert!(kernel.iter().any(|n| n == "route:list"), "{kernel:?}");
    assert!(extra.iter().any(|n| n == "queue:work"), "{extra:?}");
    assert!(
        database.iter().any(|n| n == "migrate:fresh"),
        "{database:?}"
    );

    let mut expected: Vec<String> = kernel
        .into_iter()
        .chain(extra)
        .chain(database)
        .filter(|name| is_command_shaped(name))
        .collect();
    expected.sort();
    expected.dedup();

    let mut copied: Vec<String> = RESERVED_COMMANDS.iter().map(|n| n.to_string()).collect();
    copied.sort();
    assert_eq!(
        copied, expected,
        "RESERVED_COMMANDS を、本体の `:` の無いコマンド名に合わせてください"
    );
}

#[test]
fn 型の再エクスポートの行に直し方を添える() {
    // 型が無いと rustc はこの行を「unresolved import」として行ごと見せる。
    // 中身は解析しないので、直し方は同じ行のコメントで伝える。
    let root = temp_root("reexport_hint");
    touch(&root.join("app/Models/Post.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();

    let line = code
        .lines()
        .find(|line| line.starts_with("pub use crate::__bengara_app_models_post::Post;"))
        .unwrap_or_else(|| panic!("再エクスポートの行がありません: {code}"));
    assert!(line.contains("// bengara: Post.rs に"), "{line}");
    assert!(line.contains("型 `Post`"), "{line}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 空白の入ったコマンドのファイルは控えとして飛ばす() {
    // `My Report` は `my -report` になり、`cargo artisan` では打てない。
    // 以前はここで `compile_error!` にしていたが、空白や `.` の入った名前は
    // 控えのファイル（`Copy of ...`）のことが多く、置いただけでビルドが全部
    // 止まるのが困る。警告だけ出して飛ばす。
    let root = temp_root("bad_command");
    touch(&root.join("app/Console/Commands/My Report.rs"), "");
    touch(&root.join("app/Console/Commands/SendReport.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    assert!(generator.errors.is_empty(), "{:?}", generator.errors);
    let code = generator.finish();
    assert!(!code.contains("compile_error!"), "{code}");
    // 打てない名前は一覧に入らない。打てる名前はそのまま入る。
    assert!(!code.contains("my -report"), "{code}");
    assert!(code.contains("send-report"), "{code}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 名前にできないディレクトリのエラーは1件だけ() {
    let root = temp_root("bad_dir");
    touch(&root.join("app/type/Foo.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);

    let hits = generator
        .errors
        .iter()
        .filter(|e| e.contains("予約語"))
        .count();
    assert_eq!(hits, 1, "{:?}", generator.errors);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 予約語と使えない文字は断る() {
    assert!(to_module_name("type").is_err());
    assert!(to_module_name("日本語").is_err());
    assert!(to_module_name("").is_err());
    assert!(to_module_name("_").is_err());
}

#[test]
fn 一覧は1件を1行にして直し方を添える() {
    // 以前は一覧が 1 行に並び、`up` や `DESCRIPTION` が無いときの rustc のエラーから
    // どのファイルのことか読み取れなかった。
    let root = temp_root("list_lines");
    touch(&root.join("database/migrations/2026_01_01_000000_a.rs"), "");
    touch(&root.join("database/migrations/2026_01_02_000000_b.rs"), "");
    touch(&root.join("database/seeders/UserSeeder.rs"), "");
    touch(&root.join("app/Jobs/SendWelcome.rs"), "");
    touch(&root.join("app/Console/Commands/SendReport.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();
    assert!(!code.contains("compile_error!"), "{code}");

    let migrations = list_block(&code, "pub const MIGRATIONS");
    assert_eq!(migrations.len(), 3, "{migrations:?}");
    assert!(
        migrations[1].contains("\"2026_01_01_000000_a\""),
        "{migrations:?}"
    );
    assert!(
        migrations[2].contains("\"2026_01_02_000000_b\""),
        "{migrations:?}"
    );
    assert!(
        migrations[1].ends_with(
            "// bengara: マイグレーションの 2026_01_01_000000_a.rs に \
             `pub fn up(schema: &mut Schema)` と `pub fn down(schema: &mut Schema)` を置いてください"
        ),
        "{migrations:?}"
    );

    let seeders = list_block(&code, "pub const SEEDERS");
    assert!(
        seeders[1].contains("// bengara: シーダーの UserSeeder.rs に `pub async fn run()"),
        "{seeders:?}"
    );
    let jobs = list_block(&code, "pub const JOBS");
    assert!(
        jobs[1].contains("// bengara: ジョブの SendWelcome.rs に `pub async fn handle("),
        "{jobs:?}"
    );
    let commands = list_block(&code, "pub const COMMANDS");
    assert!(
        commands[1]
            .contains("// bengara: コマンドの SendReport.rs に `pub const DESCRIPTION: &str`"),
        "{commands:?}"
    );
    assert!(
        commands[1].contains("name: \"send-report\""),
        "{commands:?}"
    );
    // 閉じかっこは自分の行にある。
    assert_eq!(
        code.lines().filter(|line| *line == "];").count(),
        4,
        "{code}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 空の一覧も形を崩さない() {
    let root = temp_root("list_empty");
    fs::create_dir_all(root.join("database/migrations")).unwrap();

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();

    assert!(
        code.contains("pub const MIGRATIONS: &[::bengara::Migration] = &[\n];\n"),
        "{code}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 設定の登録の行に直し方を添える() {
    // config/ 直下の小文字のファイルは全部 `config()` を呼ばれる。補助のファイルを
    // 置くと、rustc は OUT_DIR の中の行を指すので、同じ行に直し方を書く。
    let root = temp_root("config_hint");
    touch(&root.join("config/app.rs"), "");
    touch(&root.join("config/mail-extra.rs"), "");

    let mut generator = Generator::default();
    generator.run(&root);
    let code = generator.finish();

    let line = code
        .lines()
        .find(|line| line.contains("__registry.set(crate::config::app::config());"))
        .unwrap_or_else(|| panic!("登録の行がありません: {code}"));
    assert!(
        line.ends_with(
            "// bengara: config/app.rs に `pub fn config() -> 設定の型` を置いてください。\
             設定を返さない補助のファイルは config/ の外に置きます"
        ),
        "{line}"
    );
    // モジュール名ではなく、元のファイル名を出す。
    let line = code
        .lines()
        .find(|line| line.contains("crate::config::mail_extra::config()"))
        .unwrap_or_else(|| panic!("登録の行がありません: {code}"));
    assert!(line.contains("config/mail-extra.rs に"), "{line}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn 埋め込む置き場所はリリースビルドのときだけ見張る() {
    // デバッグビルドでも見張ると、CSS や HTML を直すたびにビルドスクリプトが走り、
    // 本体と artisan がコンパイルし直されていた（中身はディスクから読むのに）。
    let root = temp_root("watch_embedded");
    touch(&root.join("public/robots.txt"), "User-agent: *\n");
    touch(&root.join("resources/views/welcome.html"), "<p>hi</p>");

    let mut debug = Generator::default();
    debug.read_public(&root, false);
    debug.read_views(&root, false);
    assert!(debug.watched.is_empty(), "{:?}", debug.watched);
    assert!(debug.public.is_empty() && debug.views.is_empty());

    let mut release = Generator::default();
    release.read_public(&root, true);
    release.read_views(&root, true);
    assert_eq!(
        release.watched,
        [root.join("public"), root.join("resources/views")]
    );
    assert_eq!(release.public.len(), 1);
    assert_eq!(release.views.len(), 1);

    // 言語ファイルはビルド時に読むので、どちらでも見張る（ここは変えていない）。
    touch(&root.join("resources/lang/ja.toml"), "a = \"b\"\n");
    let mut debug = Generator::default();
    debug.read_lang(&root);
    assert_eq!(debug.watched, [root.join("resources/lang")]);

    let _ = fs::remove_dir_all(&root);
}
