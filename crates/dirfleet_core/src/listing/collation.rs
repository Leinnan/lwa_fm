use std::sync::OnceLock;

use icu_collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu_collator::{Collator, CollatorBorrowed};
use icu_locale_core::Locale;

static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();

/// Sorts names the way people read them in `lang`, a BCP 47 tag ("pl", "en-GB").
///
/// Case and punctuation are ignored. Shells call this once at start, before
/// any listing; later calls change nothing. Unset, English is used.
pub fn set_collator_locale(lang: &str) {
    let _ = COLLATOR.set(build(lang));
}

/// The collator listings sort with.
pub fn collator() -> &'static CollatorBorrowed<'static> {
    COLLATOR.get_or_init(|| build("en"))
}

fn build(lang: &str) -> CollatorBorrowed<'static> {
    let mut options = CollatorOptions::default();
    options.strength = Some(Strength::Primary);
    options.alternate_handling = Some(AlternateHandling::Shifted);
    let locale = Locale::try_from_str(lang)
        .or_else(|_| Locale::try_from_str("en"))
        .expect("English is a valid locale");
    Collator::try_new(locale.into(), options).expect("collation data is compiled in")
}
