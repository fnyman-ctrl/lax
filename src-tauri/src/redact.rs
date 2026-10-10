//! Masks secrets in captured text before it is stored (GUARDRAILS §4).
//!
//! Passwords, API keys, tokens, private keys, payment card numbers and Swedish
//! personal identity numbers never reach the database, even though it is local.
//! Every capture must pass its text and window title through [`redact`] before
//! storing them.
//!
//! When in doubt, mask: a masked word costs a little context, a stored secret
//! can't be taken back. Other personal data (names, emails, phone numbers) is
//! kept, because the memory needs it.
//!
//! The list of patterns draws on Mason's redact.mjs
//! (<https://github.com/Baltsar/mason>, MIT). No code was copied.

use std::sync::LazyLock;

use regex::{Captures, Regex};

pub const SECRET: &str = "[secret]";
pub const PRIVATE_KEY: &str = "[private key]";
pub const CARD_NUMBER: &str = "[card number]";
pub const PERSONAL_NUMBER: &str = "[personal identity number]";

/// Returns `text` with every secret replaced by a typed placeholder.
pub fn redact(text: &str) -> String {
    let text = PRIVATE_KEY_BLOCK.replace_all(text, PRIVATE_KEY);
    let text = SLACK_WEBHOOK.replace_all(&text, SECRET);
    let text = URL_CREDENTIALS.replace_all(&text, format!("${{scheme}}{SECRET}@"));
    let text = KNOWN_TOKEN.replace_all(&text, SECRET);
    let text = BEARER_TOKEN.replace_all(&text, |caps: &Captures| {
        if looks_like_token(&caps["token"]) {
            format!("{}{SECRET}", &caps["prefix"])
        } else {
            caps[0].to_string()
        }
    });
    let text = LABELLED_SECRET.replace_all(&text, format!("${{label}}{SECRET}"));
    let text = XML_SECRET.replace_all(&text, format!("${{open}}{SECRET}${{close}}"));
    let text = SPOKEN_PASSWORD.replace_all(&text, format!("${{label}}{SECRET}"));
    let text = PERSONAL_NUMBER_CANDIDATE.replace_all(&text, |caps: &Captures| {
        if is_personal_number(caps) {
            PERSONAL_NUMBER.to_string()
        } else {
            caps[0].to_string()
        }
    });
    let text = CARD_CANDIDATE.replace_all(&text, |caps: &Captures| mask_card_numbers(&caps[0]));
    let text = LONG_STRING.replace_all(&text, |caps: &Captures| {
        if looks_random(&caps[0]) {
            SECRET.to_string()
        } else {
            caps[0].to_string()
        }
    });
    text.into_owned()
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("redaction patterns are valid")
}

/// PEM and PGP private keys. A key cut off before its END line is masked to the
/// end of the text.
static PRIVATE_KEY_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
    )
});

static SLACK_WEBHOOK: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"https://hooks\.slack\.com/(?:services|workflows|triggers)/[A-Za-z0-9/_-]+")
});

/// `scheme://user:password@host`: the user and password are masked. The
/// password runs to the last `@`, since passwords can contain `@` and `/`.
static URL_CREDENTIALS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(?P<scheme>[a-z][a-z0-9+.-]*://)[^\s/@:]+:[^\s]*@"));

/// Tokens recognisable by their format.
static KNOWN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"\bsk-[A-Za-z0-9_-]{20,}",          // OpenAI, Anthropic and others
        r"|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",   // AWS access key IDs
        r"|\bgh[pousr]_[A-Za-z0-9]{36,}",    // GitHub
        r"|\bgithub_pat_[A-Za-z0-9_]{22,}",  // GitHub fine-grained
        r"|\bglpat-[A-Za-z0-9_-]{20,}",      // GitLab
        r"|\bxox[abposr]-[A-Za-z0-9-]{10,}", // Slack
        r"|\bAIza[0-9A-Za-z_-]{35}",         // Google
        r"|\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}", // Stripe
        r"|\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", // JWT
    ))
});

static BEARER_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)(?P<prefix>\bbearer\s+)(?P<token>[A-Za-z0-9._~+/=-]{12,})"));

/// `password: …`, `api_key = …`, `DB_PASSWORD=…`, `token: "…"`, and JSON
/// keys like `"password": "…"`. The label is kept.
static LABELLED_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)(?P<label>(?:\b|_)(?:",
        r"password|passwd|pwd|passphrase|passcode|lösenord|lösenordet|lösen",
        r"|pin|pinkod|pinkoden|pin-kod|pin-koden|pin code",
        r"|secret|client[_ -]?secret|secret[_ -]?key|private[_ -]?key",
        r"|api[_ -]?key|access[_ -]?key|access[_ -]?token|auth[_ -]?token|refresh[_ -]?token|token",
        r#")\b["']?[ \t]*(?:=>|[:=])[ \t]*)"#,
        r#"(?:"[^"\n]*"|'[^'\n]*'|[^\s,;]+)"#,
    ))
});

/// `<password>…</password>` and similar XML elements. The tags are kept.
static XML_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)(?P<open><(?:password|passwd|pwd|secret|client[_-]?secret|api[_-]?key",
        r"|access[_-]?key|secret[_-]?key|private[_-]?key|token|access[_-]?token|auth[_-]?token)>)",
        r"[^<]*(?P<close></)",
    ))
});

/// "the password is …", "lösenordet är …". Only for passwords, because
/// "the token is expired" is ordinary prose.
static SPOKEN_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)(?P<label>\b(?:password|passphrase|passcode|lösenord|lösenordet|lösen|pinkod|pinkoden)",
        r"\s+(?:is|was|är|var)\s+)",
        // Dots inside the value are kept in the mask; a final full stop isn't.
        r#"(?:"[^"\n]*"|'[^'\n]*'|[^\s,;]*[^\s,;.])"#,
    ))
});

/// YYMMDD-NNNN, YYMMDD+NNNN, YYMMDD NNNN, YYMMDDNNNN, and the same with the
/// century (YYYYMMDD-NNNN …).
/// Validated by [`is_personal_number`].
static PERSONAL_NUMBER_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"\b(?P<century>\d{2})?(?P<date>\d{6})(?P<sep>[-+ ]?)(?P<last>\d{4})\b")
});

/// 13 or more digits, optionally grouped by single spaces or hyphens. A run
/// can hold a card number next to other numbers ("4111 1111 1111 1111 12/27"),
/// so [`mask_card_numbers`] looks for the card number inside it.
static CARD_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| regex(r"\b\d(?:[ -]?\d){12,}\b"));

/// Long runs of key-like characters. Validated by [`looks_random`].
static LONG_STRING: LazyLock<Regex> = LazyLock::new(|| regex(r"[A-Za-z0-9+/_=-]{32,}"));

/// Swedish personal identity numbers (personnummer) and coordination numbers
/// (samordningsnummer, day + 60), checked by date and Luhn checksum.
fn is_personal_number(caps: &Captures) -> bool {
    if let Some(century) = caps.name("century")
        && !matches!(century.as_str(), "18" | "19" | "20")
    {
        return false;
    }
    let date = &caps["date"];
    let month: u32 = date[2..4].parse().unwrap_or(0);
    let day: u32 = date[4..6].parse().unwrap_or(0);
    let day = if day > 60 { day - 60 } else { day };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    let digits: Vec<u32> = date
        .chars()
        .chain(caps["last"].chars())
        .filter_map(|c| c.to_digit(10))
        .collect();
    luhn_valid(&digits)
}

/// Masks card numbers made of whole digit groups in `run`, such as the card
/// in "1 4111 1111 1111 1111 12". Groups that aren't part of a card are kept.
fn mask_card_numbers(run: &str) -> String {
    // Byte ranges of the digit groups between separators.
    let mut groups = Vec::new();
    let mut start = None;
    for (i, c) in run.char_indices().chain(std::iter::once((run.len(), ' '))) {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                groups.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    let mut out = String::new();
    let mut kept_until = 0;
    let mut first = 0;
    while first < groups.len() {
        // The longest card number that starts with this group. Grouped card
        // numbers are written in blocks of 3 to 6 digits (4-4-4-4, 4-6-5).
        let card_end = (first..groups.len()).rev().find(|&last| {
            let blocks_ok = first == last
                || groups[first..=last]
                    .iter()
                    .all(|&(s, e)| (3..=6).contains(&(e - s)));
            blocks_ok && is_card_number(&run[groups[first].0..groups[last].1])
        });
        match card_end {
            Some(last) => {
                out.push_str(&run[kept_until..groups[first].0]);
                out.push_str(CARD_NUMBER);
                kept_until = groups[last].1;
                first = last + 1;
            }
            None => first += 1,
        }
    }
    out.push_str(&run[kept_until..]);
    out
}

fn is_card_number(candidate: &str) -> bool {
    let digits: Vec<u32> = candidate.chars().filter_map(|c| c.to_digit(10)).collect();
    // Card numbers start with 2 to 6 (Mastercard, Amex, Diners, JCB, Visa,
    // Discover, UnionPay…). Swedish phone numbers start with 0.
    (13..=19).contains(&digits.len())
        && matches!(digits.first(), Some(2..=6))
        && luhn_valid(&digits)
}

fn luhn_valid(digits: &[u32]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                d
            }
        })
        .sum();
    !digits.is_empty() && sum.is_multiple_of(10)
}

/// A bearer token, as opposed to "Bearer authentication".
fn looks_like_token(word: &str) -> bool {
    word.chars()
        .any(|c| c.is_ascii_digit() || "._~+/=-".contains(c))
}

/// A long string that mixes upper case, lower case and digits with high
/// entropy, like a generated key. Lower-case hex (commit hashes, UUIDs),
/// file paths and readable identifiers are left alone.
fn looks_random(s: &str) -> bool {
    let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    has_upper && has_lower && has_digit && shannon_entropy(s) >= 3.5 && wordiness(s) < 0.4
}

/// The share of characters in runs of three or more lower-case letters.
/// Words, paths and camelCase names score high; generated keys score low.
fn wordiness(s: &str) -> f64 {
    let mut in_words = 0usize;
    let mut run = 0usize;
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_lowercase() {
            run += 1;
        } else {
            if run >= 3 {
                in_words += run;
            }
            run = 0;
        }
    }
    in_words as f64 / s.len() as f64
}

/// Bits per character.
fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 128];
    let mut total = 0u32;
    for b in s.bytes().filter(u8::is_ascii) {
        counts[usize::from(b)] += 1;
        total += 1;
    }
    let total = f64::from(total);
    counts
        .iter()
        .filter(|&&n| n > 0)
        .map(|&n| {
            let p = f64::from(n) / total;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    // All values below are synthetic. Personal identity numbers are made up
    // with a valid checksum, card numbers are the networks' published test
    // numbers, and keys are random strings in the right format.

    fn assert_unchanged(text: &str) {
        assert_eq!(redact(text), text);
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_unchanged("Worked on the Q3 proposal with Anna Example.");
        assert_unchanged("Möte med Anna Example om offerten på torsdag.");
        assert_unchanged("");
    }

    #[test]
    fn labelled_passwords() {
        assert_eq!(redact("password: hunter2"), "password: [secret]");
        assert_eq!(redact("Password=hunter2 next"), "Password=[secret] next");
        assert_eq!(redact(r#"pwd = "correct horse battery""#), "pwd = [secret]");
        assert_eq!(redact("Lösenord: Sommar2026!"), "Lösenord: [secret]");
        assert_eq!(redact("PIN-kod: 4821"), "PIN-kod: [secret]");
    }

    #[test]
    fn spoken_passwords() {
        assert_eq!(
            redact("The password is hunter2, ok?"),
            "The password is [secret], ok?"
        );
        assert_eq!(
            redact("Lösenordet är Sommar2026 tills vidare"),
            "Lösenordet är [secret] tills vidare"
        );
        assert_eq!(redact("lösen är fisk123."), "lösen är [secret].");
        assert_eq!(
            redact("The password is abc.Def123! ok"),
            "The password is [secret] ok"
        );
    }

    #[test]
    fn a_label_alone_is_unchanged() {
        assert_unchanged("Forgot your password?");
        assert_unchanged("Password:\nForgot your password?");
        assert_unchanged("The token is expired.");
        assert_unchanged("Glömt lösenordet?");
    }

    #[test]
    fn labelled_keys_and_tokens() {
        assert_eq!(redact("api_key=abc123def456"), "api_key=[secret]");
        assert_eq!(redact("API key: 9f8e7d6c"), "API key: [secret]");
        assert_eq!(
            redact(r#"client_secret: 'zzTopSecret'"#),
            "client_secret: [secret]"
        );
        assert_eq!(redact("token: abc.def.ghi"), "token: [secret]");
        assert_eq!(redact("DB_PASSWORD=hunter2"), "DB_PASSWORD=[secret]");
        assert_eq!(
            redact("export OPENAI_API_KEY=abc123"),
            "export OPENAI_API_KEY=[secret]"
        );
        assert_eq!(redact("csrf_token: abc123"), "csrf_token: [secret]");
        assert_eq!(
            redact(r#"{"password": "Sommar2026", "client_secret":"abc123"}"#),
            r#"{"password": [secret], "client_secret":[secret]}"#
        );
        assert_eq!(
            redact("<password>hunter2</password>"),
            "<password>[secret]</password>"
        );
        assert_eq!(redact("Lösenordet: Sommar2026"), "Lösenordet: [secret]");
        assert_eq!(redact("PIN-koden: 4821"), "PIN-koden: [secret]");
        assert_unchanged("passwordless: true");
    }

    #[test]
    fn known_token_formats() {
        // Each token is split into prefix and body so the source contains
        // nothing that secret scanners (or readers) mistake for a real key.
        let cases = [
            ("sk-proj-", "Ab3dEf6hIj9kLm2nOp5qRs8tUv"),
            ("sk-ant-", "api03-Ab3dEf6hIj9kLm2nOp5qRs8tUv"),
            ("AKIA", "IOSFODNN7EXAMPLE"),
            ("ghp_", "Ab3dEf6hIj9kLm2nOp5qRs8tUvWx1yZa4bCd"),
            ("github_pat_", "11ABCDEFG0123456789_abcdefghijklmnop"),
            ("glpat-", "Ab3dEf6hIj9kLm2nOp5q"),
            ("xoxb-", "1234567890-abcdefghij"),
            ("AIza", "SyAb3dEf6hIj9kLm2nOp5qRs8tUvWx1yZ"),
            ("sk_live_", "Ab3dEf6hIj9kLm2nOp"),
            (
                "eyJhbGciOiJIUzI1NiJ9.",
                "eyJzdWIiOiJhbm5hIn0.c2lnbmF0dXJlLXZhbHVl",
            ),
        ];
        for (prefix, body) in cases {
            let token = format!("{prefix}{body}");
            assert_eq!(
                redact(&format!("key {token} end")),
                "key [secret] end",
                "{token}"
            );
        }
    }

    #[test]
    fn bearer_tokens() {
        assert_eq!(
            redact("Authorization: Bearer abc123.def456-ghi789"),
            "Authorization: Bearer [secret]"
        );
        assert_unchanged("Use Bearer authentication for the API.");
    }

    #[test]
    fn private_keys() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAb3BlbnNzaC1rZXktdjEAAAAA\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(
            redact(&format!("before\n{key}\nafter")),
            "before\n[private key]\nafter"
        );

        let pgp =
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\nlQOYBF\n-----END PGP PRIVATE KEY BLOCK-----";
        assert_eq!(redact(pgp), "[private key]");

        let cut_off = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA";
        assert_eq!(redact(&format!("x {cut_off}")), "x [private key]");
    }

    #[test]
    fn public_keys_are_unchanged() {
        assert_unchanged("-----BEGIN PUBLIC KEY-----\nMIIBIjANBg\n-----END PUBLIC KEY-----");
    }

    #[test]
    fn credentials_in_urls() {
        assert_eq!(
            redact("postgres://anna:s3cret@db.example.com/app"),
            "postgres://[secret]@db.example.com/app"
        );
        assert_eq!(
            redact("postgres://anna:p@ss/w0rd@db.example.com/app"),
            "postgres://[secret]@db.example.com/app"
        );
        assert_unchanged("https://example.com/path?q=1");
        assert_unchanged("https://medium.com/@anna/post");
        assert_unchanged("mailto:anna@example.com");
    }

    #[test]
    fn slack_webhooks() {
        assert_eq!(
            redact(&format!(
                "POST https://hooks.slack.com/{}",
                "services/T0000/B0000/XXXXXXXX"
            )),
            "POST [secret]"
        );
    }

    #[test]
    fn personal_identity_numbers() {
        for number in [
            "811218-9876",
            "811218+9876",
            "8112189876",
            "19811218-9876",
            "198112189876",
            "811218 9876",
            "19811218 9876",
        ] {
            assert_eq!(
                redact(&format!("pnr {number} ok")),
                "pnr [personal identity number] ok",
                "{number}"
            );
        }
    }

    #[test]
    fn coordination_numbers() {
        // Day 18 + 60 = 78.
        assert_eq!(
            redact("samordningsnr 811278-9873"),
            "samordningsnr [personal identity number]"
        );
    }

    #[test]
    fn numbers_that_are_not_personal_identity_numbers() {
        assert_unchanged("811218-9875"); // wrong checksum
        assert_unchanged("811318-9876"); // month 13
        assert_unchanged("811200-9876"); // day 0
        assert_unchanged("2026-10-10"); // date
        assert_unchanged("2026-10-10 14:30"); // timestamp
        assert_unchanged("Order 123456-7890"); // order number
        assert_unchanged("070-123 45 67"); // phone number
        assert_unchanged("+46 70 123 45 67"); // phone number
        assert_unchanged("08-123 456 78"); // landline
    }

    #[test]
    fn card_numbers() {
        for number in [
            "4111111111111111",
            "4111 1111 1111 1111",
            "4111-1111-1111-1111",
            "5555 5555 5555 4444",
            "3782 822463 10005",
            "2223 0031 2200 3222",
        ] {
            assert_eq!(
                redact(&format!("card {number}.")),
                "card [card number].",
                "{number}"
            );
        }
    }

    #[test]
    fn card_numbers_next_to_other_numbers() {
        assert_eq!(
            redact("4111 1111 1111 1111 12/27 123"),
            "[card number] 12/27 123"
        );
        assert_eq!(redact("1 4111 1111 1111 1111"), "1 [card number]");
        assert_eq!(
            redact("Qty 3 5555 5555 5555 4444 2026"),
            "Qty 3 [card number] 2026"
        );
    }

    #[test]
    fn numbers_that_are_not_card_numbers() {
        assert_unchanged("4111 1111 1111 1112"); // wrong checksum
        assert_unchanged("Invoice 1234567890123"); // fails the checks
        assert_unchanged("Tracking 0012345678901234"); // starts with 0
        assert_unchanged("1 2 3 4 5 6 7 8 9 10 11 12 13");
    }

    #[test]
    fn long_random_strings() {
        for key in [
            "Zx9Qm2LpR7vT4nW8yB3kH6jD1fG5sA0c",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "q8Fz+Lk2/Vb7Rn4Tw9Yc1Hd6Ms3Pa0Xe5Gu",
        ] {
            assert_eq!(
                redact(&format!("see {key} here")),
                "see [secret] here",
                "{key}"
            );
        }
    }

    #[test]
    fn long_readable_strings_are_unchanged() {
        assert_unchanged("commit 3f1c9a7e5b2d4c6e8f0a1b3c5d7e9f1a2b4c6d8e");
        assert_unchanged("id 123e4567-e89b-12d3-a456-426614174000");
        assert_unchanged("getUserAccountSettingsForOrganization2");
        assert_unchanged("/Users/anna/Documents/Projects/Q3-proposal/draft-2026.docx");
        assert_unchanged("Rättsväsendetsinformationsförsörjningsprojektet");
    }

    #[test]
    fn several_secrets_in_one_text() {
        let text = "Anna's login: password: hunter2\npnr 811218-9876, card 4111111111111111";
        assert_eq!(
            redact(text),
            "Anna's login: password: [secret]\npnr [personal identity number], card [card number]"
        );
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact("password: hunter2 pnr 811218-9876 card 4111111111111111");
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn luhn() {
        assert!(luhn_valid(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 1, 3]));
        assert!(!luhn_valid(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 1, 4]));
        assert!(!luhn_valid(&[]));
    }
}
