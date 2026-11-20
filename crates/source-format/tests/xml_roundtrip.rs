//! XML DOM parsing, serialization, and SDR snapshot parity tests.

use std::fs;
use std::path::Path;

use busbar_sf_source_format::xml::{
    METADATA_XML_NS, XmlDocument, XmlElement, XmlNode, handle_special_entities, parse_xml,
    semantic_eq,
};
use pretty_assertions::assert_eq;

#[test]
fn round_trips_xml_comments_snapshot_exact() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../fixtures/sdr-snapshots/xmlComments/__snapshots__/verify-source-files.expected/force-app/main/default/objectTranslations/Contact-en_US/Example__c.fieldTranslation-meta.xml",
    );
    let expected = fs::read_to_string(path).expect("read fixture");
    let doc = parse_xml(&expected).expect("parse xml");
    assert_eq!(doc.to_xml_string(), expected);
}

#[test]
fn round_trips_inline_sibling_comments_in_custom_labels() {
    let input = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomLabel xmlns="http://soap.sforce.com/2006/04/metadata">
    <fullName>geTextListViewSortedBy</fullName>
    <categories>Gift Entry</categories>
    <language>en_US</language>
    <protected>true</protected>
    <shortDescription>Text to display what the table is currently sorted by</shortDescription><!-- &#8226; is the character entity reference for a center dot --><value>• Sorted By {0}</value>
</CustomLabel>
"#;
    let doc = parse_xml(input).expect("parse xml");
    assert_eq!(doc.to_xml_string(), input);
}

#[test]
fn round_trips_quotes_and_apostrophes_in_formulas() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../fixtures/sdr-snapshots/customObjects-and-children/__snapshots__/verify-source-files.expected/force-app/main/default/objects/Property__c/fields/Picture_IMG__c.field-meta.xml",
    );
    let expected = fs::read_to_string(path).expect("read fixture");
    let doc = parse_xml(&expected).expect("parse xml");
    assert_eq!(doc.to_xml_string(), expected);
}

#[test]
fn round_trips_empty_elements_and_leading_zeros_and_xsi_nil() {
    let input = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
    <code xsi:nil="true"></code>
    <zipCode>00123</zipCode>
    <searchLayouts></searchLayouts>
</CustomObject>
"#;
    let doc = parse_xml(input).expect("parse xml");
    assert_eq!(doc.to_xml_string(), input);
    assert_eq!(
        doc.root
            .child_element("zipCode")
            .and_then(XmlElement::text_content),
        Some("00123")
    );
}

#[test]
fn preserves_cdata_and_special_entities() {
    let input = r#"<?xml version="1.0" encoding="UTF-8"?>
<Sample xmlns="http://soap.sforce.com/2006/04/metadata">
    <script><![CDATA[a < b && c > d]]></script>
</Sample>
"#;
    let doc = parse_xml(input).expect("parse xml");
    assert_eq!(doc.to_xml_string(), input);
    assert_eq!(
        handle_special_entities("a &amp;#160; b &amp;#x27; c"),
        "a &#160; b &#x27; c"
    );
}

#[test]
fn extracts_unique_element_and_checks_real_values() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata"></CustomObject>"#;
    let empty_doc = parse_xml(xml).expect("parse empty object");
    assert!(!empty_doc.root.has_real_values());

    let mut field = XmlElement::new("CustomField");
    let mut name_el = XmlElement::new("field");
    name_el
        .children
        .push(XmlNode::Text("Account.Name".to_owned()));
    field.children.push(XmlNode::Element(name_el));
    assert_eq!(
        field.extract_unique_element(Some("field")),
        Some("Account.Name")
    );
    assert!(field.has_real_values());

    let doc = XmlDocument::new_metadata(field);
    assert_eq!(
        doc.root.attributes,
        vec![("xmlns".to_owned(), METADATA_XML_NS.to_owned())]
    );
}

#[test]
fn semantic_equality_ignores_sibling_order() {
    let a = parse_xml(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <fields><fullName>B</fullName></fields>
    <fields><fullName>A</fullName></fields>
</CustomObject>"#,
    )
    .expect("parse a");
    let b = parse_xml(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">
    <fields><fullName>A</fullName></fields>
    <fields><fullName>B</fullName></fields>
</CustomObject>"#,
    )
    .expect("parse b");
    assert!(semantic_eq(&a, &b));
}
