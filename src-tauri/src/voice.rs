//! Terbilang nominal dan kalimat pengumuman pembayaran untuk mesin text-to-speech.
//! Semua fungsi murni tanpa dependensi agar mudah diuji.

/// Kata bilangan 0-9 dalam Bahasa Indonesia.
const ID_DIGITS: [&str; 10] =
    ["nol", "satu", "dua", "tiga", "empat", "lima", "enam", "tujuh", "delapan", "sembilan"];

/// Kata bilangan 0-19 dalam Bahasa Inggris.
const EN_UNDER_20: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen",
    "nineteen",
];

/// Kata puluhan Bahasa Inggris; indeks 0-1 tidak dipakai.
const EN_TENS: [&str; 10] =
    ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

/// Batas atas nominal: sampai ratusan triliun, jauh di atas kebutuhan kasir.
const MAX_AMOUNT: f64 = 1e15;

/// Terbilang Indonesia untuk 1..=999 tanpa kata skala (ribu/juta/dst).
fn under_thousand_id(n: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (ratus, sisa) = (n / 100, n % 100);
    if ratus > 0 {
        parts.push(if ratus == 1 {
            "seratus".into()
        } else {
            format!("{} ratus", ID_DIGITS[ratus as usize])
        });
    }
    if (10..20).contains(&sisa) {
        parts.push(match sisa {
            10 => "sepuluh".into(),
            11 => "sebelas".into(),
            s => format!("{} belas", ID_DIGITS[s as usize - 10]),
        });
    } else {
        let (puluh, satu) = (sisa / 10, sisa % 10);
        if puluh > 0 {
            parts.push(format!("{} puluh", ID_DIGITS[puluh as usize]));
        }
        if satu > 0 {
            parts.push(ID_DIGITS[satu as usize].into());
        }
    }
    parts.join(" ")
}

/// Terbilang Indonesia, mis. 50_000 → "lima puluh ribu".
pub fn number_to_words_id(mut n: u64) -> String {
    if n == 0 {
        return "nol".into();
    }
    const SCALES: [&str; 5] = ["", " ribu", " juta", " miliar", " triliun"];
    let mut groups: Vec<u32> = Vec::new();
    while n > 0 {
        groups.push((n % 1000) as u32);
        n /= 1000;
    }
    let mut parts: Vec<String> = Vec::new();
    for i in (0..groups.len()).rev() {
        let g = groups[i];
        if g == 0 {
            continue;
        }
        // Bahasa Indonesia memakai "seribu" (tanpa skala), bukan "satu ribu"
        if g == 1 && i == 1 {
            parts.push("seribu".to_string());
        } else {
            parts.push(format!("{}{}", under_thousand_id(g), SCALES[i]));
        }
    }
    parts.join(" ")
}

/// Terbilang Inggris untuk 1..=999 tanpa kata skala.
fn under_thousand_en(n: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (ratus, sisa) = (n / 100, n % 100);
    if ratus > 0 {
        parts.push(format!("{} hundred", EN_UNDER_20[ratus as usize]));
    }
    if sisa > 0 && sisa < 20 {
        parts.push(EN_UNDER_20[sisa as usize].into());
    } else if sisa >= 20 {
        let (t, u) = (sisa / 10, sisa % 10);
        parts.push(if u == 0 {
            EN_TENS[t as usize].into()
        } else {
            format!("{}-{}", EN_TENS[t as usize], EN_UNDER_20[u as usize])
        });
    }
    parts.join(" ")
}

/// Terbilang Inggris, mis. 50_000 → "fifty thousand".
pub fn number_to_words_en(mut n: u64) -> String {
    if n == 0 {
        return "zero".into();
    }
    const SCALES: [&str; 5] = ["", " thousand", " million", " billion", " trillion"];
    let mut groups: Vec<u32> = Vec::new();
    while n > 0 {
        groups.push((n % 1000) as u32);
        n /= 1000;
    }
    let mut parts: Vec<String> = Vec::new();
    for i in (0..groups.len()).rev() {
        let g = groups[i];
        if g == 0 {
            continue;
        }
        parts.push(format!("{}{}", under_thousand_en(g), SCALES[i]));
    }
    parts.join(" ")
}

/// Dua digit desimal (0..99) tanpa nol di akhir; `None` bila nominal bulat.
fn frac_digits(amount: f64) -> Option<String> {
    let frac = ((amount - amount.trunc()) * 100.0).round() as u64;
    if frac == 0 {
        None
    } else {
        Some(format!("{frac:02}").trim_end_matches('0').to_string())
    }
}

/// Ucapkan tiap digit satu per satu: "05" → "nol lima".
fn digit_words(digits: &str, words: &[&str; 10]) -> String {
    digits
        .chars()
        .map(|c| words[c.to_digit(10).unwrap() as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

fn amount_words_id(amount: f64) -> String {
    let whole = number_to_words_id(amount.trunc() as u64);
    match frac_digits(amount) {
        None => format!("{whole} rupiah"),
        Some(d) => format!("{whole} koma {} rupiah", digit_words(&d, &ID_DIGITS)),
    }
}

fn amount_words_en(amount: f64) -> String {
    let whole = number_to_words_en(amount.trunc() as u64);
    match frac_digits(amount) {
        None => format!("{whole} rupiah"),
        Some(d) => format!("{whole} point {} rupiah", digit_words(&d, &EN_DIGITS)),
    }
}

/// Kata bilangan 0-9 dalam Bahasa Inggris (dipakai `digit_words`).
const EN_DIGITS: [&str; 10] =
    ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];

/// Susun kalimat pengumuman pembayaran berhasil. `None` bila method atau amount tidak sah.
pub fn payment_text(language: &str, method: &str, amount: f64) -> Option<String> {
    if !amount.is_finite() || !(0.0 < amount && amount < MAX_AMOUNT) {
        return None;
    }
    if language == "en" {
        let label = match method {
            "cash" => "cash",
            "edc" => "EDC card",
            "qris" => "keris",
            _ => return None,
        };
        Some(format!("Payment successful, {} {}, thank you.", label, amount_words_en(amount)))
    } else {
        let label = match method {
            "cash" => "tunai",
            "edc" => "kartu EDC",
            // ditulis "keris" agar mesin TTS melafalkan seperti senjata tradisional, bukan "kuris"
            "qris" => "keris",
            _ => return None,
        };
        Some(format!("Pembayaran berhasil, {} {}, terima kasih.", label, amount_words_id(amount)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terbilang_indonesia() {
        assert_eq!(number_to_words_id(0), "nol");
        assert_eq!(number_to_words_id(1), "satu");
        assert_eq!(number_to_words_id(10), "sepuluh");
        assert_eq!(number_to_words_id(11), "sebelas");
        assert_eq!(number_to_words_id(15), "lima belas");
        assert_eq!(number_to_words_id(20), "dua puluh");
        assert_eq!(number_to_words_id(21), "dua puluh satu");
        assert_eq!(number_to_words_id(100), "seratus");
        assert_eq!(number_to_words_id(101), "seratus satu");
        assert_eq!(number_to_words_id(110), "seratus sepuluh");
        assert_eq!(number_to_words_id(111), "seratus sebelas");
        assert_eq!(number_to_words_id(120), "seratus dua puluh");
        assert_eq!(number_to_words_id(121), "seratus dua puluh satu");
        assert_eq!(number_to_words_id(999), "sembilan ratus sembilan puluh sembilan");
        assert_eq!(number_to_words_id(1000), "seribu");
        assert_eq!(number_to_words_id(1001), "seribu satu");
        assert_eq!(number_to_words_id(1010), "seribu sepuluh");
        assert_eq!(number_to_words_id(1100), "seribu seratus");
        assert_eq!(number_to_words_id(1101), "seribu seratus satu");
        assert_eq!(number_to_words_id(2000), "dua ribu");
        assert_eq!(number_to_words_id(21000), "dua puluh satu ribu");
        assert_eq!(number_to_words_id(50000), "lima puluh ribu");
        assert_eq!(number_to_words_id(100000), "seratus ribu");
        assert_eq!(number_to_words_id(101000), "seratus satu ribu");
        assert_eq!(number_to_words_id(111111), "seratus sebelas ribu seratus sebelas");
        assert_eq!(number_to_words_id(1000000), "satu juta");
        assert_eq!(number_to_words_id(1250000), "satu juta dua ratus lima puluh ribu");
        assert_eq!(number_to_words_id(1000000000), "satu miliar");
        assert_eq!(number_to_words_id(2500000000), "dua miliar lima ratus juta");
        assert_eq!(number_to_words_id(1000000000000), "satu triliun");
        assert_eq!(
            number_to_words_id(999_999_999_999_999),
            "sembilan ratus sembilan puluh sembilan triliun sembilan ratus sembilan puluh \
             sembilan miliar sembilan ratus sembilan puluh sembilan juta sembilan ratus \
             sembilan puluh sembilan ribu sembilan ratus sembilan puluh sembilan"
        );
    }

    #[test]
    fn terbilang_inggris() {
        assert_eq!(number_to_words_en(0), "zero");
        assert_eq!(number_to_words_en(1), "one");
        assert_eq!(number_to_words_en(13), "thirteen");
        assert_eq!(number_to_words_en(15), "fifteen");
        assert_eq!(number_to_words_en(20), "twenty");
        assert_eq!(number_to_words_en(21), "twenty-one");
        assert_eq!(number_to_words_en(100), "one hundred");
        assert_eq!(number_to_words_en(101), "one hundred one");
        assert_eq!(number_to_words_en(115), "one hundred fifteen");
        assert_eq!(number_to_words_en(1000), "one thousand");
        assert_eq!(number_to_words_en(1005), "one thousand five");
        assert_eq!(number_to_words_en(2500), "two thousand five hundred");
        assert_eq!(number_to_words_en(50000), "fifty thousand");
        assert_eq!(
            number_to_words_en(123456),
            "one hundred twenty-three thousand four hundred fifty-six"
        );
        assert_eq!(number_to_words_en(1000000), "one million");
        assert_eq!(number_to_words_en(1000000000000), "one trillion");
    }

    #[test]
    fn kalimat_pembayaran() {
        assert_eq!(
            payment_text("id", "cash", 50000.0).unwrap(),
            "Pembayaran berhasil, tunai lima puluh ribu rupiah, terima kasih."
        );
        assert_eq!(
            payment_text("id", "edc", 125000.0).unwrap(),
            "Pembayaran berhasil, kartu EDC seratus dua puluh lima ribu rupiah, terima kasih."
        );
        assert_eq!(
            payment_text("id", "qris", 25000.0).unwrap(),
            "Pembayaran berhasil, keris dua puluh lima ribu rupiah, terima kasih."
        );
        assert_eq!(
            payment_text("en", "cash", 50000.0).unwrap(),
            "Payment successful, cash fifty thousand rupiah, thank you."
        );
        assert_eq!(
            payment_text("en", "qris", 7500.0).unwrap(),
            "Payment successful, keris seven thousand five hundred rupiah, thank you."
        );
        // Desimal dibaca per digit
        assert!(payment_text("id", "cash", 50000.5).unwrap().contains("koma lima rupiah"));
        assert!(payment_text("id", "cash", 50000.05).unwrap().contains("koma nol lima rupiah"));
    }

    #[test]
    fn kalimat_ditolak_bila_tidak_sah() {
        assert_eq!(payment_text("id", "transfer", 50000.0), None);
        assert_eq!(payment_text("id", "cash", 0.0), None);
        assert_eq!(payment_text("id", "cash", -1000.0), None);
        assert_eq!(payment_text("id", "cash", f64::NAN), None);
        assert_eq!(payment_text("id", "cash", MAX_AMOUNT), None);
        // Nominal di bawah batas masih sah
        assert!(payment_text("id", "cash", MAX_AMOUNT - 1.0).is_some());
    }
}
