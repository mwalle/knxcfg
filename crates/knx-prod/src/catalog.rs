//! Streaming parser for `Catalog.xml`.

use anyhow::{anyhow, Result};
use quick_xml::events::{attributes::Attribute, Event};
use quick_xml::Reader;

use crate::Product;

pub(crate) fn parse_catalog(xml: &[u8]) -> Result<(String, Vec<Product>)> {
    let mut reader = Reader::from_reader(std::io::Cursor::new(xml));
    reader.config_mut().trim_text(true);

    let mut manufacturer_id = None;
    let mut products = Vec::new();
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| anyhow!("XML error: {e}"))?;
        match event {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"Manufacturer" => {
                    if let Some(attr) = attr(&e, b"RefId") {
                        manufacturer_id = Some(attr);
                    }
                }
                b"CatalogItem" => {
                    products.push(Product {
                        id: attr(&e, b"Id").unwrap_or_default(),
                        name: attr(&e, b"Name").unwrap_or_default(),
                        number: attr(&e, b"Number").unwrap_or_default(),
                        visible_description: attr(&e, b"VisibleDescription"),
                        product_ref_id: attr(&e, b"ProductRefId"),
                        hardware2program_ref_id: attr(&e, b"Hardware2ProgramRefId"),
                    });
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    let manufacturer_id =
        manufacturer_id.ok_or_else(|| anyhow!("no Manufacturer RefId in Catalog.xml"))?;
    Ok((manufacturer_id, products))
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(attribute_value(&a));
        }
    }
    None
}

fn attribute_value(a: &Attribute) -> String {
    a.unescape_value()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(a.value.as_ref()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_catalog_item_attributes() {
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
<KNX xmlns="http://knx.org/xml/project/20">
  <ManufacturerData>
    <Manufacturer RefId="M-0083">
      <Catalog>
        <CatalogItem Id="CI-1" Name="Test Device" Number="1"
                     ProductRefId="P-1" Hardware2ProgramRefId="HP-1" />
      </Catalog>
    </Manufacturer>
  </ManufacturerData>
</KNX>"#;
        let (mid, products) = parse_catalog(xml).unwrap();
        assert_eq!(mid, "M-0083");
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].name, "Test Device");
        assert_eq!(products[0].hardware2program_ref_id.as_deref(), Some("HP-1"));
    }
}
