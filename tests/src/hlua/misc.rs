use tarantool::hlua::{LuaFunction, LuaTable};

pub fn print() {
    let lua = crate::hlua::global();

    let print: LuaFunction<_> = lua.get("print").unwrap();
    let () = print.call_with_args("hello").unwrap();
}

pub fn json() {
    let lua = crate::hlua::global();
    let require: LuaFunction<_> = lua.get("require").unwrap();
    let json: LuaTable<_> = require.call_with_args("json").unwrap();
    let encode: LuaFunction<_> = json.get("encode").unwrap();
    let mut table = std::collections::HashMap::new();
    let res: String = encode.call_with_args(vec![1, 2, 3]).unwrap();
    assert_eq!(res, "[1,2,3]");
    table.insert("a", "b");
    let res: String = encode.call_with_args(table).unwrap();
    assert_eq!(res, r#"{"a":"b"}"#);
}
