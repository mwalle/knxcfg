use knx_prod::KnxProd;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let h2p = std::env::args().nth(2).unwrap();
    let kp = KnxProd::open(&path).unwrap();
    let ap = kp.application_program(&h2p).unwrap();
    let ev = knx_prod::Evaluation::new(&ap, &Default::default());
    let mut keys: Vec<_> = ev.values().keys().collect();
    keys.sort();
    for k in keys {
        println!("{:?} = {:?}", k, ev.value(k).unwrap());
    }
}
