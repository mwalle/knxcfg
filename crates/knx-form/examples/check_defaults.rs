use std::collections::HashMap;
use knx_prod::KnxProd;
fn main() {
    for path in std::env::args().skip(1) {
        let kp = KnxProd::open(&path).unwrap();
        for p in &kp.products {
            let Some(h2p) = p.hardware2program_ref_id.clone() else { continue };
            let Ok(ap) = kp.application_program(&h2p) else { continue };
            let values = HashMap::new();
            let lang = if ap.default_language.is_empty() { "en-US" } else { &ap.default_language };
            let ev = knx_prod::Evaluation::new(&ap, &values);
            let pages = knx_form::evaluate_with(&ev, lang);
            let defaults = ev.values();
            for page in &pages {
                for section in &page.sections {
                    for f in &section.fields {
                        match defaults.get(&f.id) {
                            Some(d) if d == &f.value => {}
                            Some(d) => println!("MISMATCH {} {}: form={:?} defaults={:?}", p.number, f.id, f.value, d),
                            None => println!("MISSING {} {}: form={:?}", p.number, f.id, f.value),
                        }
                    }
                }
            }
        }
    }
}
