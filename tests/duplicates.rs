use file_organizer::{core, duplicates};
use std::fs;

fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap()
}

fn pattern() -> image::RgbImage {
    image::RgbImage::from_fn(128, 96, |x, y| {
        let v = ((x * 3 + y * 5 + (x / 20) * 40) % 256) as u8;
        image::Rgb([v, v.saturating_add(10), v.saturating_sub(10)])
    })
}

#[test]
fn groups_require_every_pair_to_match_not_transitive_chains() {
    let d = temp();
    for (name, offset) in [("a.png", 0u8), ("b.png", 7), ("c.png", 14)] {
        image::RgbImage::from_fn(32, 32, |x, y| {
            let v = (60 + ((x * 3 + y * 5) % 120)) as u8 + offset;
            image::Rgb([v, v, v])
        })
        .save(d.path().join(name))
        .unwrap();
    }
    let entries = core::scan(d.path()).unwrap();
    assert_eq!(
        duplicates::find_duplicates(&d.path().join("a.png"), &entries)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        duplicates::find_duplicates(&d.path().join("b.png"), &entries)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        duplicates::groups(&entries).unwrap(),
        vec![vec![d.path().join("a.png"), d.path().join("b.png")]]
    );
}

#[test]
fn perceptual_matches_resized_renamed_image_not_generic_semantics() {
    let d = temp();
    let image = pattern();
    image.save(d.path().join("reference.png")).unwrap();
    image::imageops::resize(&image, 64, 48, image::imageops::FilterType::Triangle)
        .save(d.path().join("renamed.png"))
        .unwrap();
    let inverted = image::RgbImage::from_fn(128, 96, |x, y| {
        let p = image.get_pixel(x, y);
        image::Rgb([255 - p[0], 255 - p[1], 255 - p[2]])
    });
    inverted.save(d.path().join("unrelated.png")).unwrap();
    image::RgbImage::from_pixel(128, 96, image::Rgb([100, 100, 100]))
        .save(d.path().join("flat.png"))
        .unwrap();
    let matches = duplicates::find_duplicates(
        &d.path().join("reference.png"),
        &core::scan(d.path()).unwrap(),
    )
    .unwrap();
    assert_eq!(matches.len(), 1, "{matches:?}");
    assert_eq!(matches[0].path, d.path().join("renamed.png"));
    assert_eq!(matches[0].kind, "perceptual");
    assert!(matches[0].distance <= 8);
}

#[test]
fn exact_duplicates_include_corrupt_images_and_nonimages() {
    let d = temp();
    fs::write(d.path().join("a.png"), b"not a valid image").unwrap();
    fs::write(d.path().join("b.bin"), b"not a valid image").unwrap();
    fs::write(d.path().join("c.png"), b"different").unwrap();
    let entries = core::scan(d.path()).unwrap();
    let matches = duplicates::find_duplicates(&d.path().join("a.png"), &entries).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, d.path().join("b.bin"));
    assert_eq!(matches[0].kind, "exact");
    assert_eq!(matches[0].distance, 0);
}
