//! What an assistant's text changes in what is written, word by word, for
//! the composer to show in place of the text until the user accepts or
//! rejects it (`assistant_edit.rs`).

/// What one stretch of the text does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Op {
    Same,
    Gone,
    New,
}

/// Past this many token pairs the two texts are not compared word by word:
/// the one goes and the other comes, whole. Bounds the table to a few MB.
const PAIRS_MAX: usize = 2_000_000;

/// `s` as words, runs of whitespace, and every other character alone.
fn tokens(s: &str) -> Vec<&str> {
    let class = |c: char| {
        if c.is_whitespace() {
            1
        } else if c.is_alphanumeric() || c == '_' {
            0
        } else {
            2
        }
    };
    let mut out = Vec::new();
    let mut start = 0;
    let mut last = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        if i > start && (Some(k) != last || k == 2) {
            out.push(&s[start..i]);
            start = i;
        }
        last = Some(k);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `old` turned into `new`: what stays, what goes and what comes, in
/// order, with what goes before what comes where both are at one place.
pub(super) fn diff(old: &str, new: &str) -> Vec<(Op, String)> {
    let a = tokens(old);
    let b = tokens(new);
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);

    let mut ops: Vec<(Op, &str)> = a[..pre].iter().map(|t| (Op::Same, *t)).collect();
    if am.len().saturating_mul(bm.len()) > PAIRS_MAX {
        ops.extend(am.iter().map(|t| (Op::Gone, *t)));
        ops.extend(bm.iter().map(|t| (Op::New, *t)));
    } else {
        ops.extend(lcs(am, bm));
    }
    ops.extend(a[a.len() - suf..].iter().map(|t| (Op::Same, *t)));

    // A space kept between two changes reads as part of one change.
    let mut k = 1;
    while k + 1 < ops.len() {
        if ops[k].0 == Op::Same
            && ops[k].1.trim().is_empty()
            && ops[k - 1].0 != Op::Same
            && ops[k + 1].0 != Op::Same
        {
            ops[k].0 = Op::Gone;
            ops.insert(k + 1, (Op::New, ops[k].1));
            k += 1;
        }
        k += 1;
    }

    // Each run of changes as what goes, then what comes.
    let mut out: Vec<(Op, String)> = Vec::new();
    let mut push = |op: Op, text: &str| match out.last_mut() {
        Some((last, s)) if *last == op => s.push_str(text),
        _ => out.push((op, text.to_string())),
    };
    let mut k = 0;
    while k < ops.len() {
        if ops[k].0 == Op::Same {
            push(Op::Same, ops[k].1);
            k += 1;
            continue;
        }
        let end = (k..ops.len())
            .find(|&j| ops[j].0 == Op::Same)
            .unwrap_or(ops.len());
        for op in [Op::Gone, Op::New] {
            for (o, t) in &ops[k..end] {
                if *o == op {
                    push(op, t);
                }
            }
        }
        k = end;
    }
    out
}

/// The longest common run of `a` and `b`, as the steps from one to the
/// other.
fn lcs<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(Op, &'a str)> {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut t = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if a[i] == b[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((Op::Same, a[i]));
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            out.push((Op::Gone, a[i]));
            i += 1;
        } else {
            out.push((Op::New, b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|t| (Op::Gone, *t)));
    out.extend(b[j..].iter().map(|t| (Op::New, *t)));
    out
}

/// The diff as HTML for a block that keeps white space as typed: what
/// goes in `<del>`, what comes in `<ins>`.
pub(super) fn diff_html(old: &str, new: &str) -> String {
    let mut html = String::new();
    for (op, text) in diff(old, new) {
        let text = escape(&text);
        match op {
            Op::Same => html.push_str(&text),
            Op::Gone => html.push_str(&format!("<del>{text}</del>")),
            Op::New => html.push_str(&format!("<ins>{text}</ins>")),
        }
    }
    html
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(old: &str, new: &str) -> String {
        diff(old, new)
            .into_iter()
            .map(|(op, t)| match op {
                Op::Same => t,
                Op::Gone => format!("[-{t}-]"),
                Op::New => format!("{{+{t}+}}"),
            })
            .collect()
    }

    #[test]
    fn words_that_change_are_marked_and_the_rest_kept() {
        assert_eq!(
            show("Hi Ada, see you Monday.", "Hi Ada, see you on Tuesday!"),
            "Hi Ada, see you [-Monday.-]{+on Tuesday!+}"
        );
        assert_eq!(show("same", "same"), "same");
    }

    #[test]
    fn a_space_between_changes_joins_them() {
        assert_eq!(show("a b c", "x y z"), "[-a b c-]{+x y z+}");
    }

    #[test]
    fn all_new_or_all_gone() {
        assert_eq!(show("", "Hello"), "{+Hello+}");
        assert_eq!(show("Hello\n\nBye", ""), "[-Hello\n\nBye-]");
    }

    #[test]
    fn the_html_is_escaped() {
        assert_eq!(
            diff_html("Tom & <b>", "Tom & \"q\""),
            "Tom &amp; <del>&lt;b&gt;</del><ins>&quot;q&quot;</ins>"
        );
    }
}
