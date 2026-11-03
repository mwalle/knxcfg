use knx_prod::KnxProd;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let h2p = std::env::args().nth(2).unwrap();
    let kp = KnxProd::open(&path).unwrap();
    let ap = kp.application_program(&h2p).unwrap();
    let defaults = knx_prod::parameter_defaults(&ap);
    let mut keys: Vec<_> = defaults.keys().collect();
    keys.sort();
    for k in keys {
        println!("{:?} = {:?}", k, defaults.get(k).unwrap());
    }
}
