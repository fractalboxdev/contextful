use contextful_core::connector::reference::Hydrated;

fn main() {
    let credential = Hydrated::new("secret");
    let _copy = credential.clone();
}
