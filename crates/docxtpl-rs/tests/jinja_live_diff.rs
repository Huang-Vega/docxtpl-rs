//! Live table-driven Jinja2 differential for compatibility added outside the DOCX corpus.

#![cfg(feature = "oracle")]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use docxtpl_template::render_core_properties;
use docxtpl_xml::{XmlDocument, XmlLimits};
use serde_json::{json, Value};

const DC_NS: &str = "http://purl.org/dc/elements/1.1/";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn python() -> String {
    std::env::var("DOCXTPL_PYTHON").unwrap_or_else(|_| "python".to_string())
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn rust_render(template: &str, context: &Value) -> Result<String, String> {
    let xml = format!(
        concat!(
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>",
            "<cp:coreProperties",
            " xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\"",
            " xmlns:dc=\"http://purl.org/dc/elements/1.1/\">",
            "<dc:title>{}</dc:title>",
            "</cp:coreProperties>"
        ),
        xml_text(template)
    );
    let output = render_core_properties(&xml, context, false).map_err(|error| {
        error
            .kind()
            .map(|kind| kind.oracle_exception())
            .unwrap_or("XmlError")
            .to_string()
    })?;
    let doc = XmlDocument::parse_strict(&output, &XmlLimits::default())
        .map_err(|_| "XmlError".to_string())?;
    let title = doc
        .descendants(doc.root())
        .into_iter()
        .find(|&node| {
            doc.tag(node)
                .is_some_and(|tag| tag.ns == DC_NS && tag.local == "title")
        })
        .expect("core renderer preserves dc:title");
    Ok(doc.element_text(title).unwrap_or("").to_string())
}

fn python_results(cases: &Value) -> Vec<Value> {
    let script = root().join("tests/oracle/jinja_probe.py");
    let mut child = Command::new(python())
        // Force UTF-8 for the JSON pipe as well as stdout.  Windows otherwise
        // decodes stdin with the active console code page before json.load.
        .args(["-X", "utf8"])
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start pinned Python Jinja oracle");
    child
        .stdin
        .take()
        .expect("oracle stdin")
        .write_all(&serde_json::to_vec(cases).expect("serialize differential cases"))
        .expect("write oracle cases");
    let output = child.wait_with_output().expect("wait for Jinja oracle");
    assert!(
        output.status.success(),
        "Jinja oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout)
        .expect("parse Jinja oracle JSON")
        .as_array()
        .expect("oracle result is an array")
        .clone()
}

#[test]
fn added_jinja_surface_matches_live_jinja2() {
    let cases = json!([
        {
            "id": "mapping_methods",
            "template": "{% for k,v in mapping.items() %}{{k}}={{v}};{% endfor %}|{{mapping.keys()|join(',')}}|{{mapping.values()|join(',')}}|{{mapping.get('x')}}|{{mapping.get('z','D')}}",
            "context": {"mapping": {"x": 1, "y": 2}}
        },
        {
            "id": "string_methods",
            "template": "{{text.split()}}|{{duplicate.split(None,1)}}|{{'a,b,c'.split(sep=',',maxsplit=1)}}|{{text.startswith('  ab')}}|{{text.endswith('  ')}}|{{'abcabc'.count('ab')}}|{{'éxé'.index('é',1)}}|{{'abc'.startswith(('x','ab'))}}",
            "context": {"text": "  ab cd  ", "duplicate": "a a b"}
        },
        {
            "id": "sequence_methods",
            "template": "{{values.count(2)}}|{{values.index(2)}}|{{values.index(2,2)}}|{{mixed.count(1)}}|{{mixed.index(1)}}",
            "context": {"values": [1, 2, 2], "mixed": [true, 1, 1.0]}
        },
        {
            "id": "deterministic_filters",
            "template": "[{{'abc'|center(6)}}]|{{1000|filesizeformat}}|{{-1500|filesizeformat}}|{{0.5|filesizeformat}}|{{true|filesizeformat}}|{{1024|filesizeformat(true)}}|{{'<p>Hello&nbsp; <b>world</b></p>'|striptags}}|{{'foo bar baz qux'|truncate(9)}}|{{'foo bar baz qux'|truncate(9,true)}}|{{'Hello, world! 42'|wordcount}}|{{'one two three'|wordwrap(7)}}",
            "context": {}
        },
        {
            "id": "html5_entities_and_type_tests",
            "template": "{{'<b>&CounterClockwiseContourIntegral; &nbsp;</b>'|striptags}}|{{true is number}}/{{'text' is sequence}}/{{mapping is sequence}}/{{7 is sequence}}",
            "context": {"mapping": {"x": 1}}
        },
        {
            "id": "wordwrap_boundaries",
            "template": "{{hyphen|wordwrap(6,true,'/',true)}}|{{hyphen|wordwrap(6,true,'/',false)}}|{{longword|wordwrap(4,false,'/')}}|{{cjk|wordwrap(4)}}",
            "context": {"hyphen": "alpha-beta gamma", "longword": "abcdefgh ij", "cjk": "你好世界 测试"}
        },
        {
            "id": "stateful_globals_and_callable",
            "template": "{% set c=cycler('odd','even') %}{{c.current}}/{{c.next()}}/{{c.next()}}/{{c.current}}/{{c.reset() or '-'}}/{{c.current}}|{% set j=joiner('|') %}{{j()}}a{{j()}}b{{j()}}c|{{range is callable}}/{{c is callable}}/{{j is callable}}/{{missing is callable}}",
            "context": {}
        },
        {
            "id": "macro_namespace_filter_and_call_blocks",
            "template": "{% macro item(value,prefix='>') %}{{prefix}}{{value}}{% endmacro %}{{item('A')}}|{% set ns=namespace(total=0) %}{% for value in values %}{% set ns.total=ns.total+value %}{% endfor %}{{ns.total}}|{% filter upper %}mixed{% endfilter %}|{% macro wrap() %}[{{caller()}}]{% endmacro %}{% call wrap() %}inside{% endcall %}|{{item is callable}}/{{ns is callable}}",
            "context": {"values": [1, 2, 3]}
        },
        {
            "id": "split_empty_value_error",
            "template": "{{'abc'.split('')}}",
            "context": {}
        },
        {
            "id": "index_missing_value_error",
            "template": "{{[1,2].index(3)}}",
            "context": {}
        },
        {
            "id": "wordwrap_width_value_error",
            "template": "{{'abc'|wordwrap(0)}}",
            "context": {}
        },
        {
            "id": "xmlattr_name_value_error",
            "template": "{{attrs|xmlattr}}",
            "context": {"attrs": {"bad\u{000b}key": "value"}}
        },
        {
            "id": "urlencode_pair_value_error",
            "template": "{{pairs|urlencode}}",
            "context": {"pairs": [[1]]}
        },
        {
            "id": "format_character_value_error",
            "template": "{{'%q'|format(1)}}",
            "context": {}
        },
        {
            "id": "mapping_format",
            "template": "{{'%(name)s/%(count)04d'|format(name='Ada', count=7)}}",
            "context": {}
        },
        {
            "id": "filesize_value_error",
            "template": "{{'not-a-number'|filesizeformat}}",
            "context": {}
        }
    ]);

    let oracle = python_results(&cases);
    for (case, expected) in cases.as_array().expect("cases array").iter().zip(oracle) {
        let id = case["id"].as_str().expect("case id");
        assert_eq!(expected["id"], id, "oracle order changed");
        let actual = rust_render(
            case["template"].as_str().expect("template string"),
            &case["context"],
        );
        let actual = actual.as_ref().map(String::as_str).map_err(String::as_str);
        match (expected["output"].as_str(), expected["error"].as_str()) {
            (Some(output), None) => assert_eq!(actual, Ok(output), "case {id}"),
            (None, Some(error)) => assert_eq!(actual, Err(error), "case {id}"),
            _ => panic!("{id}: malformed oracle result {expected}"),
        }
    }
}

#[test]
fn known_vm_deviations_are_explicitly_gated() {
    let cases = json!([{
        "id": "negative_divisor_modulo",
        "template": "{{5 % -3}}",
        "context": {}
    }]);
    let oracle = python_results(&cases);
    assert_eq!(oracle[0]["output"], "-1");
    assert_eq!(rust_render("{{5 % -3}}", &json!({})), Ok("2".to_string()));
}
