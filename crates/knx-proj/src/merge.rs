//! Merging product data documents of one manufacturer (`Catalog.xml`,
//! `Hardware.xml` from several knxprods) into one.
//!
//! Elements are matched by name and their `Id`, `Identifier`, `RefId` or
//! (for `Translation`) `AttributeName`; an element without one of these is
//! matched by its name. Children of a matched element are merged the same
//! way, the others are appended. A single document is returned unchanged.

use anyhow::{bail, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};

enum Node {
    Element(Element),
    /// Text, comments and other events, written back as they were.
    Other(Event<'static>),
}

struct Element {
    start: BytesStart<'static>,
    children: Vec<Node>,
}

impl Element {
    fn key(&self) -> (Vec<u8>, Option<Vec<u8>>) {
        let name = self.start.name().as_ref().to_vec();
        let id = ["Id", "Identifier", "RefId", "AttributeName"].iter().find_map(|a| {
            self.start.try_get_attribute(*a).ok().flatten().map(|v| v.value.into_owned())
        });
        (name, id)
    }

    fn merge(&mut self, other: Element) {
        for child in other.children {
            match child {
                Node::Element(e) => {
                    let key = e.key();
                    match self.children.iter_mut().find_map(|c| match c {
                        Node::Element(mine) if mine.key() == key => Some(mine),
                        _ => None,
                    }) {
                        Some(mine) => mine.merge(e),
                        None => self.children.push(Node::Element(e)),
                    }
                }
                // Whitespace and comments of the other document are dropped.
                Node::Other(_) => {}
            }
        }
    }
}

/// The prolog (declaration etc.) and the root element of a document.
fn parse(data: &[u8]) -> Result<(Vec<Event<'static>>, Element)> {
    let mut reader = Reader::from_reader(data);
    let mut prolog = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf)?.into_owned();
        match event {
            Event::Start(start) => stack.push(Element { start, children: Vec::new() }),
            Event::Empty(start) => {
                let e = Element { start, children: Vec::new() };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(e)),
                    None => return Ok((prolog, e)),
                }
            }
            Event::End(_) => {
                let e = stack.pop().expect("reader checks nesting");
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(e)),
                    None => return Ok((prolog, e)),
                }
            }
            Event::Eof => bail!("document has no root element"),
            other => match stack.last_mut() {
                Some(parent) => parent.children.push(Node::Other(other)),
                None => prolog.push(other),
            },
        }
        buf.clear();
    }
}

fn write(w: &mut Writer<Vec<u8>>, e: &Element) -> Result<()> {
    if e.children.is_empty() {
        w.write_event(Event::Empty(e.start.borrow()))?;
        return Ok(());
    }
    w.write_event(Event::Start(e.start.borrow()))?;
    for c in &e.children {
        match c {
            Node::Element(child) => write(w, child)?,
            Node::Other(ev) => w.write_event(ev.borrow())?,
        }
    }
    w.write_event(Event::End(e.start.to_end()))?;
    Ok(())
}

/// Merge `docs` into the first one.
pub fn merge_documents(docs: &[Vec<u8>]) -> Result<Vec<u8>> {
    match docs {
        [] => bail!("nothing to merge"),
        [one] => return Ok(one.clone()),
        _ => {}
    }
    let bom = docs[0].starts_with(b"\xef\xbb\xbf");
    let strip = |d: &[u8]| if d.starts_with(b"\xef\xbb\xbf") { d[3..].to_vec() } else { d.to_vec() };
    let (prolog, mut root) = parse(&strip(&docs[0]))?;
    for d in &docs[1..] {
        let (_, other) = parse(&strip(d))?;
        if other.key() != root.key() {
            bail!("the documents have different root elements");
        }
        root.merge(other);
    }
    let mut out = Vec::new();
    if bom {
        out.extend_from_slice(b"\xef\xbb\xbf");
    }
    let mut w = Writer::new(out);
    for ev in &prolog {
        w.write_event(ev.borrow())?;
    }
    write(&mut w, &root)?;
    Ok(w.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_by_id() {
        let a = br#"<?xml version="1.0"?>
<KNX><ManufacturerData><Manufacturer RefId="M-1"><Hardware><Hardware Id="H-1" Name="a"/></Hardware>
<Languages><Language Identifier="de-DE"><TranslationUnit RefId="H-1"><TranslationElement RefId="H-1"><Translation AttributeName="Name" Text="A"/></TranslationElement></TranslationUnit></Language></Languages>
</Manufacturer></ManufacturerData></KNX>"#;
        let b = br#"<?xml version="1.0"?>
<KNX><ManufacturerData><Manufacturer RefId="M-1"><Hardware><Hardware Id="H-1" Name="a"/><Hardware Id="H-2" Name="b"/></Hardware>
<Languages><Language Identifier="de-DE"><TranslationUnit RefId="H-2"><TranslationElement RefId="H-2"><Translation AttributeName="Name" Text="B"/></TranslationElement></TranslationUnit></Language></Languages>
</Manufacturer></ManufacturerData></KNX>"#;
        let merged = String::from_utf8(merge_documents(&[a.to_vec(), b.to_vec()]).unwrap()).unwrap();
        assert_eq!(merged.matches("<Hardware Id=\"H-1\"").count(), 1, "{merged}");
        assert_eq!(merged.matches("<Hardware Id=\"H-2\"").count(), 1, "{merged}");
        assert_eq!(merged.matches("<Language ").count(), 1, "{merged}");
        assert_eq!(merged.matches("<TranslationUnit ").count(), 2, "{merged}");
        assert!(merged.starts_with("<?xml version=\"1.0\"?>"));
    }
}
