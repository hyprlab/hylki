//! Scripts written without spaces between words. In Chinese, Japanese, Thai
//! and their neighbours, and after Korean words, a link sits right against
//! the text around it, so these characters end a link and may start one
//! (#684).

/// A letter or punctuation mark of a script written without spaces: none of
/// them belongs in a link written out in plain text.
pub(crate) fn is_unspaced(c: char) -> bool {
    matches!(c,
        '\u{0E00}'..='\u{0EFF}' // Thai, Lao
        | '\u{1000}'..='\u{109F}' // Myanmar
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
        | '\u{1780}'..='\u{17FF}' // Khmer
        | '\u{3000}'..='\u{303F}' // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
        | '\u{3130}'..='\u{318F}' // Hangul Compatibility Jamo
        | '\u{31F0}'..='\u{31FF}' // Katakana Phonetic Extensions
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{FF00}'..='\u{FFEF}' // Halfwidth and Fullwidth Forms
        | '\u{20000}'..='\u{3FFFF}' // CJK Extensions B onwards
    )
}
