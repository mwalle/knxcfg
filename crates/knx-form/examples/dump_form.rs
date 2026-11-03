use std::collections::HashMap;
use knx_prod::KnxProd;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let h2p = std::env::args().nth(2).unwrap();
    let kp = KnxProd::open(&path).unwrap();
    let ap = kp.application_program(&h2p).unwrap();
    let values = HashMap::new();
    let lang = if ap.default_language.is_empty() { "en-US" } else { &ap.default_language };
    let pages = knx_form::evaluate(&ap, &values, lang);
    for page in &pages {
        for section in &page.sections {
            for f in &section.fields {
                println!("{} = {:?}", f.id, f.value);
            }
        }
    }
}
