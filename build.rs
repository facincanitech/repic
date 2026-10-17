// Coloca o ícone e o nome do programa dentro do RePic.exe (Explorer, barra de
// tarefas, Menu Iniciar, "Adicionar ou remover programas").
fn main() {
    println!("cargo:rerun-if-changed=assets/repic.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/repic.ico")
            .set("ProductName", "RePic")
            .set("FileDescription", "RePic")
            .set("CompanyName", "FacincaniTech")
            .set("LegalCopyright", "FacincaniTech");
        if let Err(e) = res.compile() {
            // Sem o compilador de recursos do Windows SDK o exe sai sem ícone,
            // mas funciona igual.
            println!("cargo:warning=ícone não embutido: {e}");
        }
    }
}
