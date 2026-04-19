use crate::effects::{Effect, EffectSet};
use crate::stage::property::Property;
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("text_split")
            .input(NType::record([
                ("text", NType::Text),
                ("delimiter", NType::Text),
            ]))
            .output(NType::List(Box::new(NType::Text)))
            .pure()
            .description("Split text by a delimiter into a list of strings")
            .example(json!({"text": "a,b,c", "delimiter": ","}), json!(["a", "b", "c"]))
            .example(json!({"text": "hello world", "delimiter": " "}), json!(["hello", "world"]))
            .example(json!({"text": "one", "delimiter": ","}), json!(["one"]))
            .example(json!({"text": "", "delimiter": ","}), json!([""]))
            .example(json!({"text": "a::b::c", "delimiter": "::"}), json!(["a", "b", "c"]))
            .tag("text").tag("string").tag("pure")
            .alias("split").alias("str_split").alias("explode")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_join")
            .input(NType::record([
                ("items", NType::List(Box::new(NType::Text))),
                ("delimiter", NType::Text),
            ]))
            .output(NType::Text)
            .pure()
            .description("Join a list of strings with a delimiter")
            .example(json!({"items": ["a", "b", "c"], "delimiter": ","}), json!("a,b,c"))
            .example(json!({"items": ["hello", "world"], "delimiter": " "}), json!("hello world"))
            .example(json!({"items": ["one"], "delimiter": ","}), json!("one"))
            .example(json!({"items": [], "delimiter": ","}), json!(""))
            .example(json!({"items": ["a", "b"], "delimiter": ""}), json!("ab"))
            .tag("text").tag("string").tag("pure")
            .alias("join").alias("implode").alias("concat_list")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("regex_match")
            .input(NType::record([
                ("text", NType::Text),
                ("pattern", NType::Text),
            ]))
            .output(NType::record([
                ("matched", NType::Bool),
                ("groups", NType::List(Box::new(NType::Text))),
                ("full_match", NType::optional(NType::Text)),
            ]))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Match text against a regex pattern; fails on invalid regex")
            .example(
                json!({"text": "hello123", "pattern": "(\\d+)"}),
                json!({"matched": true, "groups": ["123"], "full_match": "123"}),
            )
            .example(
                json!({"text": "abc", "pattern": "\\d+"}),
                json!({"matched": false, "groups": [], "full_match": null}),
            )
            .example(
                json!({"text": "2024-01-15", "pattern": "(\\d{4})-(\\d{2})-(\\d{2})"}),
                json!({"matched": true, "groups": ["2024", "01", "15"], "full_match": "2024-01-15"}),
            )
            .example(
                json!({"text": "test@email.com", "pattern": "(.+)@(.+)"}),
                json!({"matched": true, "groups": ["test", "email.com"], "full_match": "test@email.com"}),
            )
            .example(
                json!({"text": "no match", "pattern": "^\\d+$"}),
                json!({"matched": false, "groups": [], "full_match": null}),
            )
            .tag("text").tag("regex").tag("pure")
            .alias("regexp").alias("re_match").alias("pattern_match")
            .property(Property::SetMember {
                field: "output.matched".into(),
                set: vec![json!(true), json!(false)],
            })
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("regex_replace")
            .input(NType::record([
                ("text", NType::Text),
                ("pattern", NType::Text),
                ("replacement", NType::Text),
            ]))
            .output(NType::Text)
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Replace regex matches in text; fails on invalid regex")
            .example(json!({"text": "hello 123 world", "pattern": "\\d+", "replacement": "NUM"}), json!("hello NUM world"))
            .example(json!({"text": "aaa", "pattern": "a", "replacement": "b"}), json!("bbb"))
            .example(json!({"text": "foo bar", "pattern": "\\s+", "replacement": "_"}), json!("foo_bar"))
            .example(json!({"text": "no match", "pattern": "\\d+", "replacement": "X"}), json!("no match"))
            .example(json!({"text": "abc", "pattern": "(.)", "replacement": "[$1]"}), json!("[a][b][c]"))
            .tag("text").tag("regex").tag("pure")
            .alias("re_replace").alias("regexp_sub").alias("substitute")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_template")
            .input(NType::record([
                ("template", NType::Text),
                ("variables", NType::Map {
                    key: Box::new(NType::Text),
                    value: Box::new(NType::Text),
                }),
            ]))
            .output(NType::Text)
            .pure()
            .description("Interpolate variables into a template string using {{key}} syntax")
            .example(json!({"template": "Hello, {{name}}!", "variables": {"name": "Alice"}}), json!("Hello, Alice!"))
            .example(json!({"template": "{{a}} + {{b}}", "variables": {"a": "1", "b": "2"}}), json!("1 + 2"))
            .example(json!({"template": "no vars", "variables": {}}), json!("no vars"))
            .example(json!({"template": "{{x}}", "variables": {"x": "value"}}), json!("value"))
            .example(json!({"template": "{{a}}{{a}}", "variables": {"a": "x"}}), json!("xx"))
            .tag("text").tag("template").tag("pure")
            .alias("interpolate").alias("mustache").alias("handlebars")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_hash")
            .input(NType::record([
                ("text", NType::Text),
                ("algorithm", NType::optional(NType::Text)),
            ]))
            .output(NType::record([
                ("hash", NType::Text),
                ("algorithm", NType::Text),
            ]))
            .effects(EffectSet::new([Effect::Fallible, Effect::NonDeterministic]))
            .description("Compute a cryptographic hash of text; defaults to SHA-256")
            .example(json!({"text": "hello", "algorithm": "sha256"}), json!({"hash": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824", "algorithm": "sha256"}))
            .example(json!({"text": "hello", "algorithm": null}), json!({"hash": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824", "algorithm": "sha256"}))
            .example(json!({"text": "", "algorithm": "sha256"}), json!({"hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "algorithm": "sha256"}))
            .example(json!({"text": "test", "algorithm": "md5"}), json!({"hash": "098f6bcd4621d373cade4e832627b4f6", "algorithm": "md5"}))
            .example(json!({"text": "abc", "algorithm": "sha256"}), json!({"hash": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "algorithm": "sha256"}))
            .tag("text").tag("crypto").tag("hash").tag("pure")
            .alias("sha256").alias("md5").alias("checksum").alias("digest")
            .build_stdlib(key)
            .unwrap(),
        // ── New text manipulation stages ───────────────────────────────────────
        StageBuilder::new("text_upper")
            .input(NType::Text)
            .output(NType::Text)
            .pure()
            .description("Convert text to uppercase")
            .example(json!("hello"), json!("HELLO"))
            .example(json!("World"), json!("WORLD"))
            .example(json!("foo BAR"), json!("FOO BAR"))
            .example(json!(""), json!(""))
            .example(json!("123abc"), json!("123ABC"))
            .tag("text").tag("string").tag("pure")
            .alias("uppercase").alias("upcase").alias("to_upper")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_lower")
            .input(NType::Text)
            .output(NType::Text)
            .pure()
            .description("Convert text to lowercase")
            .example(json!("HELLO"), json!("hello"))
            .example(json!("World"), json!("world"))
            .example(json!("FOO BAR"), json!("foo bar"))
            .example(json!(""), json!(""))
            .example(json!("123ABC"), json!("123abc"))
            .tag("text").tag("string").tag("pure")
            .alias("lowercase").alias("downcase").alias("to_lower")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_trim")
            .input(NType::Text)
            .output(NType::Text)
            .pure()
            .description("Remove leading and trailing whitespace from text")
            .example(json!("  hello  "), json!("hello"))
            .example(json!("\thello\n"), json!("hello"))
            .example(json!("no spaces"), json!("no spaces"))
            .example(json!(""), json!(""))
            .example(json!("  "), json!(""))
            .tag("text").tag("string").tag("pure")
            .alias("strip").alias("trim_whitespace")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_length")
            .input(NType::Text)
            .output(NType::Number)
            .pure()
            .description("Return the number of characters in a text string")
            .example(json!("hello"), json!(5.0))
            .example(json!(""), json!(0.0))
            .example(json!("abc"), json!(3.0))
            .example(json!("hello world"), json!(11.0))
            .example(json!("αβγ"), json!(3.0))
            .tag("text").tag("string").tag("pure")
            .alias("strlen").alias("len").alias("count_chars").alias("char_count")
            .property(Property::Range {
                field: "output".into(),
                min: Some(0.0),
                max: None,
            })
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_contains")
            .input(NType::record([
                ("text", NType::Text),
                ("substring", NType::Text),
            ]))
            .output(NType::Bool)
            .pure()
            .description("Check if text contains a substring; case-sensitive")
            .example(json!({"text": "hello world", "substring": "world"}), json!(true))
            .example(json!({"text": "hello world", "substring": "xyz"}), json!(false))
            .example(json!({"text": "hello", "substring": ""}), json!(true))
            .example(json!({"text": "", "substring": "x"}), json!(false))
            .example(json!({"text": "Hello", "substring": "hello"}), json!(false))
            .tag("text").tag("string").tag("pure")
            .alias("includes").alias("has_substring").alias("str_contains")
            .property(Property::SetMember {
                field: "output".into(),
                set: vec![json!(true), json!(false)],
            })
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_reverse")
            .input(NType::Text)
            .output(NType::Text)
            .pure()
            .description("Reverse the characters in a text string")
            .example(json!("hello"), json!("olleh"))
            .example(json!("abc"), json!("cba"))
            .example(json!(""), json!(""))
            .example(json!("a"), json!("a"))
            .example(json!("racecar"), json!("racecar"))
            .tag("text").tag("string").tag("pure")
            .alias("reverse_string").alias("strrev")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("text_replace")
            .input(NType::record([
                ("text", NType::Text),
                ("from", NType::Text),
                ("to", NType::Text),
            ]))
            .output(NType::Text)
            .pure()
            .description("Replace all literal occurrences of a substring in text")
            .example(json!({"text": "hello world", "from": "world", "to": "Rust"}), json!("hello Rust"))
            .example(json!({"text": "aaa", "from": "a", "to": "b"}), json!("bbb"))
            .example(json!({"text": "no match", "from": "xyz", "to": "abc"}), json!("no match"))
            .example(json!({"text": "foo.bar.baz", "from": ".", "to": "/"}), json!("foo/bar/baz"))
            .example(json!({"text": "hello", "from": "", "to": "x"}), json!("hello"))
            .tag("text").tag("string").tag("pure")
            .alias("str_replace").alias("replace_all").alias("substitute_literal")
            .build_stdlib(key)
            .unwrap(),
    ]
}
