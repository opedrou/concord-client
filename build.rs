// Ícone do `.exe` no Explorer e na barra de tarefas. Só quando o alvo é Windows: o Linux
// não tem recurso embutido no binário. Regerar o ícone: `cargo test -- --ignored gera_icone`.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("ícone do .exe");
    }
}
