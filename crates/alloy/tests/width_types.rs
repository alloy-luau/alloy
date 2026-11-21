//! The width types: what the artifact holds under the flag, how a
//! remote packs one, and what the source reports.

use alloy::EmitOptions;

const ON: &str = "AlloyExperimentalWidthTypes = true";

fn compiled(src: &str, fflags: &str) -> alloy::Output {
    let options = EmitOptions {
        fflags: toml::from_str(fflags).unwrap(),
        ..EmitOptions::default()
    };

    alloy::compile_with(src, &options).unwrap()
}

fn messages(out: &alloy::Output) -> Vec<&str> {
    out.diagnostics.iter().map(|d| d.message.as_str()).collect()
}

#[test]
fn a_width_type_is_a_number_under_its_name() {
    let out = compiled(
        "local hp: u8 = 5\nlocal x: f32? = nil\nprint(hp + 1, x)\n",
        ON,
    );

    assert!(messages(&out).is_empty(), "{:?}", messages(&out));
    assert!(
        out.ship.starts_with("type u8 = number type f32 = number "),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), 3, "{}", out.ship);
}

#[test]
fn a_type_of_the_file_comes_first() {
    let out = compiled("type u8 = string\nlocal hp: u8 = \"a\"\nprint(hp)\n", ON);

    assert!(!out.ship.contains("type u8 = number"), "{}", out.ship);
}

#[test]
fn the_flag_off_leaves_the_name_alone() {
    let out = compiled("local hp: u8 = 5\nprint(hp)\n", "");

    assert!(!out.ship.contains("type u8"), "{}", out.ship);
}

#[test]
fn a_width_type_packs_as_its_attribute_does() {
    let typed = compiled(
        "struct Shot\n    x: f32\n    power: u8\nend\nremote Fire(shot: Shot, id: u16?) from client\n",
        ON,
    );
    let attributed = compiled(
        "struct Shot\n    @f32 x: number\n    @u8 power: number\nend\nremote Fire(shot: Shot, @u16 id: number?) from client\n",
        "",
    );

    assert!(messages(&typed).is_empty(), "{:?}", messages(&typed));

    let layout = |luau: &str| {
        luau.lines()
            .filter(|l| l.contains("\"u16"))
            .map(|l| l[l.find("\"u16").unwrap()..].to_string())
            .collect::<Vec<_>>()
    };

    assert!(typed.ship.contains("\"f32"), "{}", typed.ship);
    assert!(typed.ship.contains("\"u8"), "{}", typed.ship);
    assert!(!layout(&typed.ship).is_empty(), "{}", typed.ship);
    assert_eq!(layout(&typed.ship), layout(&attributed.ship));
}

#[test]
fn a_default_must_fit_the_width_type() {
    let out = compiled(
        "struct Hit\n    hp: u8 = 300\nend\nremote Heal(amount: i8 = 200) from client\nprint(Hit)\n",
        ON,
    );

    assert_eq!(
        messages(&out),
        vec![
            "`u8` holds a whole number from 0 to 255; the default of `hp`, 300, does not fit",
            "`i8` holds a whole number from -128 to 127; the default of `amount`, 200, does not fit",
        ]
    );
}

/// `Net` and `Width` are modules of the std that are not final as a
/// whole, so each call reports under its own flag.
#[test]
fn a_std_module_that_is_not_final_needs_its_flag() {
    let src = "import { Net } from \"@alloy/std/net\"\nimport { Width } from \"@alloy/std/width\"\nprint(Net.get(\"x\"), Width.size(\"u8\"))\n";
    let off = compiled(src, "");

    assert_eq!(
        messages(&off),
        vec![
            "`Net.get` is not final; set `AlloyExperimentalNetLibrary = true` under `[fflags]` in alloy.toml to use it",
            "`Width.size` is not final; set `AlloyExperimentalWidthTypes = true` under `[fflags]` in alloy.toml to use it",
        ]
    );

    let on = compiled(
        src,
        "AlloyExperimentalNetLibrary = true\nAlloyExperimentalWidthTypes = true",
    );

    assert!(messages(&on).is_empty(), "{:?}", messages(&on));
    assert!(on.ship.contains("__alloy.Net.get(\"x\")"), "{}", on.ship);
}
