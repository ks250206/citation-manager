use cm_core::papers::{NewPaper, PaperLibrary};
use cm_domain::ExternalId;
use cm_storage::{LibraryDb, LibraryDirectory};
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let dir = LibraryDirectory::prepare(root).unwrap();
    let mut db = LibraryDb::open(&dir).unwrap();
    for value in [
        "https://doi.org/10.1002/abc%3C493%3E",
        "https://doi.org/10.1002/query%3C493%3E?download=1#part",
        "https://doi.org/10.1002/double%253C493%253E",
        "https://doi.org/10.1002/invalid%ZZ",
        "https://doi.org/10.1002/utf8%FF",
        "https://doi.org/10.1002/space%20tail",
    ] {
        let mut input = NewPaper::titled("Old API synthetic paper");
        input.bibliography.journal = Some("Preserved journal".into());
        input.external_ids.push(ExternalId {
            namespace: "DOI".into(),
            value: value.into(),
        });
        let paper = db.papers().create(input).unwrap();
        println!("{} {}", paper.id.0, paper.bibliography.doi.unwrap());
    }
}
