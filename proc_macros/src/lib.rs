// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

extern crate proc_macro;
extern crate proc_macro2;

use heck::{ToSnakeCase, ToUpperCamelCase};
use quote::{format_ident, quote};
use std::{collections::HashMap, env};
use syn::parse_macro_input;

// ============================================================================
// Types and Constants
// ============================================================================

type TomlStructMap = HashMap<String, proc_macro2::TokenStream>;

const OPTIONAL_COMMENT_PREFIX: &str = "#:Opt:#";

// ============================================================================
// Utility Functions
// ============================================================================

/// Validates that all elements in a TOML array have the same type
fn validate_array_has_same_type(array: &toml::value::Array) -> bool {
    if array.is_empty() {
        return true;
    }

    let first_type = &array[0];
    array
        .iter()
        .all(|x| std::mem::discriminant(x) == std::mem::discriminant(first_type))
}

/// Extracts optional keys from TOML string comments
fn extract_optional_keys(root_name: &str, toml_str: &str) -> Vec<String> {
    toml_str
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(OPTIONAL_COMMENT_PREFIX)
                .map(str::trim)
        })
        .map(|key| format!("{root_name}.{key}"))
        .collect()
}

/// Replaces optional comment markers with regular comments
fn replace_optional_comment(orig: &str) -> String {
    let mut lines = orig.lines().peekable();
    let mut result = Vec::new();

    while let Some(line) = lines.next() {
        if line.trim().strip_prefix(OPTIONAL_COMMENT_PREFIX).is_some() {
            result.push("# Optional".to_owned());
            for next_line in lines.by_ref() {
                let trimmed = next_line.trim();
                if trimmed.strip_prefix('#').is_some() {
                    result.push(trimmed.to_owned());
                    continue;
                }
                if !trimmed.is_empty() {
                    result.push(format!("# {trimmed}"));
                    break;
                }
            }
        } else {
            result.push(line.trim().to_owned());
        }
    }
    result.join("\n") + "\n"
}

// ============================================================================
// Type Determination
// ============================================================================

/// Determines the Rust type for a TOML value
fn determine_type(
    val: &toml::Value,
    structs: &mut TomlStructMap,
    key: &str,
    optional_keys: &[String],
    prefix: &str,
) -> proc_macro2::TokenStream {
    match val {
        toml::Value::String(_) => quote! { String },
        toml::Value::Integer(_) => quote! { i64 },
        toml::Value::Boolean(_) => quote! { bool },
        toml::Value::Array(arr) => {
            assert!(!arr.is_empty(), "Array must have at least one element");
            assert!(
                validate_array_has_same_type(arr),
                "All array elements must have the same type"
            );
            let value_type = determine_type(&arr[0], structs, key, optional_keys, prefix);
            quote! { Vec<#value_type> }
        }
        toml::Value::Table(_) => {
            let struct_name = format_ident!("{}", key.to_upper_camel_case());
            traverse_toml(val, key, structs, optional_keys, prefix);
            quote! { #struct_name }
        }
        _ => panic!("Unsupported TOML type"),
    }
}

// ============================================================================
// Field Generation
// ============================================================================

/// Generates struct fields from a TOML table
fn generate_fields(
    value: &toml::Value,
    structs: &mut TomlStructMap,
    optional_keys: &[String],
    prefix: &str,
) -> proc_macro2::TokenStream {
    let mut fields = proc_macro2::TokenStream::new();

    if let toml::Value::Table(table) = value {
        for (key, val) in table {
            let field_name = format_ident!("{}", key);
            let field_tree = format!("{prefix}.{key}");
            let is_optional = optional_keys.contains(&field_tree);

            let field_type = determine_type(val, structs, key, optional_keys, prefix);
            let field_type = if is_optional {
                quote! { Option<#field_type> }
            } else {
                field_type
            };

            fields.extend(quote! {
                pub #field_name: #field_type,
            });
        }
    }
    fields
}

// ============================================================================
// Proxy Getter Generation
// ============================================================================

/// Generates proxy getter methods for nested fields
fn generate_proxy_getters(
    table: &toml::value::Table,
    optional_keys: &[String],
    new_prefix: &str,
) -> proc_macro2::TokenStream {
    let mut proxy_getters = proc_macro2::TokenStream::new();

    for (section, val) in table {
        add_proxy_getters(
            &mut proxy_getters,
            val,
            &[section.to_string()],
            &[format_ident!("{}", section)],
            optional_keys,
            &format!("{new_prefix}."),
        );
    }

    proxy_getters
}

/// Recursively adds proxy getters for nested structures
fn add_proxy_getters(
    proxy_getters: &mut proc_macro2::TokenStream,
    val: &toml::Value,
    field_path: &[String],
    field_idents: &[proc_macro2::Ident],
    optional_keys: &[String],
    prefix: &str,
) {
    if let toml::Value::Table(table) = val {
        // Recursively process nested tables
        for (k, v) in table {
            let mut new_path = field_path.to_owned();
            new_path.push(k.to_string());
            let mut new_idents = field_idents.to_owned();
            new_idents.push(format_ident!("{}", k));

            add_proxy_getters(
                proxy_getters,
                v,
                &new_path,
                &new_idents,
                optional_keys,
                prefix,
            );
        }
    } else {
        // Generate getter for leaf fields
        if field_path.is_empty() || field_idents.is_empty() {
            return;
        }

        let getter_name = create_getter_name(field_path);
        let field_access = create_field_access(field_idents);
        let field_tree = format!("{prefix}{}", field_path.join("."));
        let return_type = determine_leaf_type(val, field_path, optional_keys, prefix);
        let is_optional = optional_keys.contains(&field_tree);

        let getter = if is_optional {
            quote! {
                pub fn #getter_name(&self) -> Option<&#return_type> {
                    #field_access.as_ref()
                }
            }
        } else {
            quote! {
                pub fn #getter_name(&self) -> &#return_type {
                    &#field_access
                }
            }
        };

        proxy_getters.extend(getter);
    }
}

/// Creates a getter method name from field path
fn create_getter_name(field_path: &[String]) -> proc_macro2::Ident {
    format_ident!(
        "get_{}",
        field_path
            .iter()
            .map(|s| s.to_snake_case())
            .collect::<Vec<_>>()
            .join("_")
    )
}

/// Creates field access chain for getter methods
fn create_field_access(field_idents: &[proc_macro2::Ident]) -> proc_macro2::TokenStream {
    let mut access = quote! { self };
    for ident in field_idents {
        access = quote! { #access.#ident };
    }
    access
}

/// Determines the type for a leaf value in getter methods
fn determine_leaf_type(
    val: &toml::Value,
    field_path: &[String],
    optional_keys: &[String],
    prefix: &str,
) -> proc_macro2::TokenStream {
    let mut dummy_structs = HashMap::new();
    determine_type(
        val,
        &mut dummy_structs,
        field_path.last().unwrap(),
        optional_keys,
        prefix,
    )
}

// ============================================================================
// Struct Generation
// ============================================================================

/// Generates a struct definition with optional implementation block
fn generate_struct(
    name: &str,
    fields: &proc_macro2::TokenStream,
    proxy_getters: Option<proc_macro2::TokenStream>,
) -> proc_macro2::TokenStream {
    let struct_name = format_ident!("{}", name.to_upper_camel_case());
    if let Some(getters) = proxy_getters {
        quote! {
            #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
            #[cfg_attr(test, derive(Default))]
            pub struct #struct_name {
                #fields
            }

            impl #struct_name {
                #getters
            }
        }
    } else {
        quote! {
            #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
            #[cfg_attr(test, derive(Default))]
            pub struct #struct_name {
                #fields
            }
        }
    }
}

// ============================================================================
// TOML Traversal
// ============================================================================

/// Traverses TOML structure and generates corresponding Rust structs
fn traverse_toml(
    value: &toml::Value,
    name: &str,
    structs: &mut TomlStructMap,
    optional_keys: &[String],
    prefix: &str,
) {
    let new_prefix = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    };

    let toml::Value::Table(table) = value else {
        panic!("Root value must be a table");
    };

    let fields = generate_fields(value, structs, optional_keys, &new_prefix);
    let is_root = prefix.is_empty();

    let item_struct = if is_root {
        // Root struct gets proxy getters for all nested fields
        let proxy_getters = generate_proxy_getters(table, optional_keys, &new_prefix);
        generate_struct(name, &fields, Some(proxy_getters))
    } else {
        // Non-root structs only get the struct definition
        generate_struct(name, &fields, None)
    };

    structs.insert(name.to_string(), item_struct);
}

// ============================================================================
// Main Proc Macro
// ============================================================================

/// Main procedural macro that converts TOML file to Rust structs
///
/// # Panics
///
/// This function will panic if:
/// - `CARGO_MANIFEST_DIR` environment variable is not set
/// - The input filename is invalid
/// - The TOML file cannot be read
/// - The TOML content is invalid
#[proc_macro]
pub fn toml_file_to_struct(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(item as syn::LitStr);
    let filename = input.value();

    // Resolve file path
    let root_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR environment variable is not set.");
    let toml_path = std::path::Path::new(&root_dir).join(filename);

    // Extract struct name from filename
    let stem = toml_path.file_stem().unwrap_or_else(|| {
        panic!(
            "Invalid file name ({}). Should be <name>.toml",
            toml_path.display()
        )
    });

    let root_struct_name = stem
        .to_str()
        .unwrap_or_else(|| {
            panic!(
                "Cannot read file stem. Check filename({}) contains unsupported character",
                toml_path.display()
            )
        })
        .to_upper_camel_case();

    // Read and parse TOML file
    let toml_str = std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|_| panic!("Failed to read file. ({})", toml_path.display()));

    let optional_keys = extract_optional_keys(&root_struct_name, &toml_str);
    let parsed_toml: toml::Value = toml::from_str(&toml_str).expect("Invalid TOML");

    // Generate structs
    let mut structs: TomlStructMap = HashMap::new();
    traverse_toml(
        &parsed_toml,
        &root_struct_name,
        &mut structs,
        &optional_keys,
        "",
    );

    // Combine all generated tokens
    let mut tokens = proc_macro::TokenStream::new();
    for s in structs.values() {
        let s: proc_macro::TokenStream = s.clone().into();
        tokens.extend(s);
    }

    // Generate print function
    let print_struct_ident = format_ident!("print_{}", root_struct_name.to_snake_case());
    let replaced_toml_str = replace_optional_comment(&toml_str);

    let print_struct_token: proc_macro::TokenStream = quote! {
        pub fn #print_struct_ident() -> String {
            #replaced_toml_str.to_owned()
        }
    }
    .into();

    tokens.extend(print_struct_token);
    tokens
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_array_has_same_type() {
        let homogeneous_arr = vec![
            toml::Value::Integer(1),
            toml::Value::Integer(2),
            toml::Value::Integer(3),
        ];
        assert!(validate_array_has_same_type(&homogeneous_arr));

        let heterogeneous_arr = vec![
            toml::Value::Integer(1),
            toml::Value::String("2".to_owned()),
            toml::Value::Integer(3),
        ];
        assert!(!validate_array_has_same_type(&heterogeneous_arr));

        let empty_arr = vec![];
        assert!(validate_array_has_same_type(&empty_arr));
    }

    #[test]
    fn test_determine_type() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";

        // Test basic types
        let test_cases = vec![
            (toml::Value::String("test".to_owned()), "String"),
            (toml::Value::Integer(1), "i64"),
            (toml::Value::Boolean(true), "bool"),
        ];

        for (val, expected) in test_cases {
            let result = determine_type(&val, &mut structs, "test", &optional_keys, prefix);
            assert_eq!(result.to_string(), expected);
        }

        // Test array type
        let val = toml::Value::Array(vec![toml::Value::Integer(1)]);
        let result = determine_type(&val, &mut structs, "test", &optional_keys, prefix);
        assert_eq!(result.to_string(), "Vec < i64 >");

        // Test table type
        let val = toml::Value::Table(toml::value::Table::new());
        let result = determine_type(&val, &mut structs, "test", &optional_keys, prefix);
        assert_eq!(result.to_string(), "Test");
    }

    #[test]
    #[should_panic(expected = "All array elements must have the same type")]
    fn test_determine_type_heterogeneous_array_panic() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";
        let val = toml::Value::Array(vec![
            toml::Value::Integer(1),
            toml::Value::String("mixed".to_owned()),
        ]);
        determine_type(&val, &mut structs, "test", &optional_keys, prefix);
    }

    #[test]
    #[should_panic(expected = "Unsupported TOML type")]
    fn test_determine_type_unsupported_type() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";
        let val = toml::Value::Float(42.7); // Arbitrary float value for testing
        determine_type(&val, &mut structs, "test", &optional_keys, prefix);
    }

    #[test]
    fn test_generate_fields() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";

        // Test empty table
        let val = toml::Value::Table(toml::value::Table::new());
        let result = generate_fields(&val, &mut structs, &optional_keys, prefix);
        assert_eq!(result.to_string(), "");

        // Test table with fields
        let test_cases = vec![
            (
                toml::Value::String("test".to_owned()),
                "pub test : String ,",
            ),
            (toml::Value::Integer(1), "pub test : i64 ,"),
            (toml::Value::Boolean(true), "pub test : bool ,"),
        ];

        for (toml_val, expected) in test_cases {
            let mut table = toml::value::Table::new();
            table.insert("test".to_owned(), toml_val);
            let val = toml::Value::Table(table);
            let result = generate_fields(&val, &mut structs, &optional_keys, prefix);
            assert_eq!(result.to_string(), expected);
        }
    }

    #[test]
    fn test_generate_fields_with_optional() {
        let mut structs = HashMap::new();
        let optional_keys = vec!["TestPrefix.optional_field".to_string()];
        let prefix = "TestPrefix";

        let mut table = toml::value::Table::new();
        table.insert(
            "optional_field".to_owned(),
            toml::Value::String("test".to_owned()),
        );
        table.insert("required_field".to_owned(), toml::Value::Integer(42));
        let val = toml::Value::Table(table);

        let result = generate_fields(&val, &mut structs, &optional_keys, prefix);
        let result_str = result.to_string();

        // Should contain both optional and required fields
        assert!(result_str.contains("pub optional_field : Option < String >"));
        assert!(result_str.contains("pub required_field : i64"));
    }

    #[test]
    fn test_generate_fields_non_table() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";

        // Test with non-table value - should return empty TokenStream
        let val = toml::Value::String("not a table".to_owned());
        let result = generate_fields(&val, &mut structs, &optional_keys, prefix);
        assert_eq!(result.to_string(), "");
    }

    #[test]
    fn test_create_getter_name() {
        let field_path = vec!["nested".to_string(), "field_name".to_string()];
        let result = create_getter_name(&field_path);
        assert_eq!(result.to_string(), "get_nested_field_name");

        let single_field = vec!["simple".to_string()];
        let result = create_getter_name(&single_field);
        assert_eq!(result.to_string(), "get_simple");
    }

    #[test]
    fn test_create_field_access() {
        let field_idents = vec![format_ident!("first"), format_ident!("second")];
        let result = create_field_access(&field_idents);
        assert_eq!(result.to_string(), "self . first . second");

        let single_ident = vec![format_ident!("single")];
        let result = create_field_access(&single_ident);
        assert_eq!(result.to_string(), "self . single");
    }

    #[test]
    fn test_determine_leaf_type() {
        let field_path = vec!["test".to_string()];
        let optional_keys = vec![];
        let prefix = "";

        let val = toml::Value::String("test".to_owned());
        let result = determine_leaf_type(&val, &field_path, &optional_keys, prefix);
        assert_eq!(result.to_string(), "String");

        let val = toml::Value::Array(vec![toml::Value::Boolean(true)]);
        let result = determine_leaf_type(&val, &field_path, &optional_keys, prefix);
        assert_eq!(result.to_string(), "Vec < bool >");
    }

    #[test]
    fn test_generate_proxy_getters() {
        let mut table = toml::value::Table::new();
        table.insert("field1".to_owned(), toml::Value::String("value".to_owned()));
        table.insert("field2".to_owned(), toml::Value::Integer(42));

        let optional_keys = vec![];
        let prefix = "Root";

        let result = generate_proxy_getters(&table, &optional_keys, prefix);
        let result_str = result.to_string();

        assert!(result_str.contains("pub fn get_field1"));
        assert!(result_str.contains("pub fn get_field2"));
    }

    #[test]
    fn test_add_proxy_getters_empty_paths() {
        let mut proxy_getters = proc_macro2::TokenStream::new();
        let val = toml::Value::String("test".to_owned());

        // Test with empty field_path - should return early
        add_proxy_getters(
            &mut proxy_getters,
            &val,
            &[], // empty path
            &[],
            &[],
            "prefix.",
        );

        assert_eq!(proxy_getters.to_string(), "");

        // Test with empty field_idents - should return early
        add_proxy_getters(
            &mut proxy_getters,
            &val,
            &["field".to_string()],
            &[], // empty idents
            &[],
            "prefix.",
        );

        assert_eq!(proxy_getters.to_string(), "");
    }

    #[test]
    fn test_add_proxy_getters_with_optional_fields() {
        let mut proxy_getters = proc_macro2::TokenStream::new();
        let val = toml::Value::String("test".to_owned());
        let optional_keys = vec!["prefix.test_field".to_string()];

        add_proxy_getters(
            &mut proxy_getters,
            &val,
            &["test_field".to_string()],
            &[format_ident!("test_field")],
            &optional_keys,
            "prefix.",
        );

        let result_str = proxy_getters.to_string();
        assert!(result_str.contains("Option"));
        assert!(result_str.contains("as_ref"));
    }

    #[test]
    fn test_generate_struct_with_getters() {
        let fields = quote! { pub field: String, };
        let getters = Some(quote! { pub fn get_field(&self) -> &String { &self.field } });

        let result = generate_struct("test", &fields, getters);
        let result_str = result.to_string();

        assert!(result_str.contains("pub struct Test"));
        assert!(result_str.contains("impl Test"));
        assert!(result_str.contains("pub fn get_field"));
    }

    #[test]
    fn test_generate_struct_without_getters() {
        let fields = quote! { pub field: String, };

        let result = generate_struct("test", &fields, None);
        let result_str = result.to_string();

        assert!(result_str.contains("pub struct Test"));
        assert!(!result_str.contains("impl Test"));
    }

    #[test]
    #[should_panic(expected = "Root value must be a table")]
    fn test_traverse_toml_non_table_panic() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let val = toml::Value::String("not a table".to_owned());

        traverse_toml(&val, "test", &mut structs, &optional_keys, "");
    }

    #[test]
    fn test_traverse_toml_nested_prefix() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];

        let mut inner_table = toml::value::Table::new();
        inner_table.insert(
            "inner_field".to_owned(),
            toml::Value::String("value".to_owned()),
        );
        let val = toml::Value::Table(inner_table);

        traverse_toml(&val, "nested", &mut structs, &optional_keys, "parent");

        assert!(structs.contains_key("nested"));
        // Should not have impl block since it's not root
        let struct_code = structs.get("nested").unwrap().to_string();
        assert!(!struct_code.contains("impl"));
    }

    #[test]
    fn test_extract_optional_keys() {
        let root_name = "Root";
        let toml_str = r#"
            field1 = "value1"
            field2 = "value2"
            #:Opt:# field3
            field4 = "value4"
            #:Opt:# field5
        "#;
        let result = extract_optional_keys(root_name, toml_str);
        assert_eq!(result, vec!["Root.field3", "Root.field5"]);
    }

    #[test]
    fn test_extract_optional_keys_empty_input() {
        let result = extract_optional_keys("Root", "");
        assert_eq!(result, Vec::<String>::new());

        let result = extract_optional_keys("Root", "no optional markers here");
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    #[should_panic(expected = "Array must have at least one element")]
    fn test_empty_array_panic() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";
        let val = toml::Value::Array(vec![]);
        determine_type(&val, &mut structs, "test", &optional_keys, prefix);
    }

    #[test]
    fn test_replace_optional_comment() {
        let toml_str = r#"
            field1 = "value1"
            #:Opt:# field3
            field3 = "value3"
            #:Opt:# field4
            # field4 is optional
            field4 = "value4"
            #:Opt:# field5
        "#;
        let result = replace_optional_comment(toml_str);
        assert_eq!(
            result,
            r#"
field1 = "value1"
# Optional
# field3 = "value3"
# Optional
# field4 is optional
# field4 = "value4"
# Optional
"#
        );
    }

    #[test]
    fn test_replace_optional_comment_edge_cases() {
        // Test with no optional comments
        let toml_str = "field1 = \"value1\"\nfield2 = \"value2\"";
        let result = replace_optional_comment(toml_str);
        assert_eq!(result, "field1 = \"value1\"\nfield2 = \"value2\"\n");

        // Test with consecutive optional comment markers (no content between them)
        let toml_str = "#:Opt:# field1\n#:Opt:# field2";
        let result = replace_optional_comment(toml_str);
        // The second #:Opt:# line will be treated as the content for the first marker
        assert_eq!(result, "# Optional\n#:Opt:# field2\n");

        // Test with separated optional comment markers
        let toml_str = "#:Opt:# field1\nfield1 = \"value\"\n#:Opt:# field2\nfield2 = \"value\"";
        let result = replace_optional_comment(toml_str);
        assert_eq!(
            result,
            "# Optional\n# field1 = \"value\"\n# Optional\n# field2 = \"value\"\n"
        );

        // Test empty string
        let result = replace_optional_comment("");
        assert_eq!(result, "\n");
    }

    #[test]
    fn test_nested_array_types() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";

        // Test nested array with strings
        let val = toml::Value::Array(vec![toml::Value::String("test".to_owned())]);
        let result = determine_type(&val, &mut structs, "test", &optional_keys, prefix);
        assert_eq!(result.to_string(), "Vec < String >");

        // Test nested array with booleans
        let val = toml::Value::Array(vec![toml::Value::Boolean(true)]);
        let result = determine_type(&val, &mut structs, "test", &optional_keys, prefix);
        assert_eq!(result.to_string(), "Vec < bool >");
    }

    #[test]
    fn test_traverse_toml_with_nested_structures() {
        let mut structs = HashMap::new();
        let optional_keys = vec![];
        let prefix = "";

        // Create nested TOML structure
        let mut subsub_table = toml::value::Table::new();
        subsub_table.insert("leaf_field".to_owned(), toml::Value::Boolean(true));

        let mut sub_table = toml::value::Table::new();
        sub_table.insert("subsub".to_owned(), toml::Value::Table(subsub_table));

        let mut nested_table = toml::value::Table::new();
        nested_table.insert("sub".to_owned(), toml::Value::Table(sub_table));
        nested_table.insert(
            "nested_field".to_owned(),
            toml::Value::String("abc".to_owned()),
        );

        let mut table = toml::value::Table::new();
        table.insert("nested".to_owned(), toml::Value::Table(nested_table));
        table.insert(
            "field1".to_owned(),
            toml::Value::String("value1".to_owned()),
        );
        table.insert("field2".to_owned(), toml::Value::Integer(42));

        let val = toml::Value::Table(table);
        let name = "root";

        traverse_toml(&val, name, &mut structs, &optional_keys, prefix);

        assert!(structs.contains_key(name));
        let struct_code = structs.get(name).unwrap().to_string();

        // Verify proxy getters are generated
        assert!(struct_code.contains("pub fn get_field1"));
        assert!(struct_code.contains("pub fn get_field2"));
        assert!(struct_code.contains("pub fn get_nested_nested_field"));
        assert!(struct_code.contains("pub fn get_nested_sub_subsub_leaf_field"));

        // Verify nested structs don't have impl blocks
        if let Some(nested_code) = structs.get("nested") {
            let nested_code = nested_code.to_string();
            assert!(!nested_code.contains("impl Nested"));
        }
    }
}
