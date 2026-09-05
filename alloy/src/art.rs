//! The logo art and its brand gradient. The art is drawn by hand from
//! assets/aly-symbol.png: the A and the diamond.

use crate::ui::{BRAND, RESET, fg};

/// The logo with the brand gradient. See [`render_gradient`].
pub fn logo(color: bool) -> String {
    render_gradient(LOGO, color)
}

/// The color of one gradient row. The ramp goes from pale lavender,
/// through the brand purple, to deep violet.
pub fn row_color(row: usize, rows: usize) -> (u8, u8, u8) {
    const TOP: (u8, u8, u8) = (0xEC, 0xE4, 0xFF);
    const BOTTOM: (u8, u8, u8) = (0x4A, 0x30, 0xA8);

    let last = rows.saturating_sub(1).max(1) as u32;
    let t = row.min(rows.saturating_sub(1)) as u32 * 1000 / last;
    let mix = |a: u8, b: u8, t: u32| -> u8 {
        let a = a as i32;
        let b = b as i32;

        (a + (b - a) * t as i32 / 500) as u8
    };

    if t <= 500 {
        (
            mix(TOP.0, BRAND.0, t),
            mix(TOP.1, BRAND.1, t),
            mix(TOP.2, BRAND.2, t),
        )
    } else {
        (
            mix(BRAND.0, BOTTOM.0, t - 500),
            mix(BRAND.1, BOTTOM.1, t - 500),
            mix(BRAND.2, BOTTOM.2, t - 500),
        )
    }
}

/// Paints the art with the gradient. A denser ramp character is
/// brighter. The output is plain text when color is off.
fn render_gradient(art: &str, color: bool) -> String {
    let art = art.trim_matches('\n');

    if !color {
        return art.to_owned();
    }

    fn density(c: char) -> Option<u16> {
        match c {
            '.' => Some(45),

            ':' | '-' => Some(60),

            '=' | '+' => Some(78),

            '*' | '#' => Some(92),

            '%' | '@' => Some(100),

            _ => None,
        }
    }

    let lines: Vec<&str> = art.lines().collect();
    let rows = lines.len();
    let mut out = String::with_capacity(art.len() * 3);
    let mut current: Option<(u8, u8, u8)> = None;

    for (row, line) in lines.iter().enumerate() {
        let row_rgb = row_color(row, rows);

        for ch in line.chars() {
            let want = density(ch).map(|pct| {
                let scale = |v: u8| (v as u16 * pct / 100) as u8;
                (scale(row_rgb.0), scale(row_rgb.1), scale(row_rgb.2))
            });

            if want != current {
                match want {
                    Some(rgb) => out.push_str(&fg(rgb)),

                    None => out.push_str(RESET),
                }

                current = want;
            }

            out.push(ch);
        }

        if current.is_some() {
            out.push_str(RESET);
            current = None;
        }

        out.push('\n');
    }

    out.pop();

    out
}

pub const LOGO: &str = r#"
                  .+.
                 +###+
                *#####*
               *#######*
              *###+:+###*
             *###:   :###*
            *###:     :###*
           *###:       :###*
          *###:         :###*
         *###:    .+.    :###*
        *###:    +###+    :###*
       *###:    *#####*    :###*
      *###:    *#######*    :###*
     *###*      *#####*      *###*
     +***+       +###+       +***+
                  .+.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_logo_has_no_escapes() {
        assert!(!logo(false).contains('\x1b'));
        assert_eq!(logo(false).lines().count(), 16);
    }

    #[test]
    fn gradient_ends_meet_the_ramp() {
        assert_eq!(row_color(0, 10), (0xEC, 0xE4, 0xFF));
        assert_eq!(row_color(9, 10), (0x4A, 0x30, 0xA8));
    }
}
