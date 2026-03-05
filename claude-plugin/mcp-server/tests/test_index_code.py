"""Tests for scripts/index-code.py pure functions."""

from __future__ import annotations

import importlib
import sys
from pathlib import Path

# Add scripts to path so we can import the module
_scripts_dir = Path(__file__).resolve().parent.parent.parent.parent / "scripts"
_mcp_src = Path(__file__).resolve().parent.parent / "src"
sys.path.insert(0, str(_scripts_dir))
sys.path.insert(0, str(_mcp_src))

index_code = importlib.import_module("index-code")


class TestExtToLang:
    def test_rust(self):
        assert index_code._ext_to_lang(".rs") == "rust"

    def test_python(self):
        assert index_code._ext_to_lang(".py") == "python"

    def test_go(self):
        assert index_code._ext_to_lang(".go") == "go"

    def test_typescript(self):
        assert index_code._ext_to_lang(".ts") == "typescript"
        assert index_code._ext_to_lang(".tsx") == "typescript"

    def test_unknown(self):
        assert index_code._ext_to_lang(".java") is None
        assert index_code._ext_to_lang(".cpp") is None


class TestShouldSkip:
    def test_skip_git(self):
        assert index_code._should_skip(Path(".git/config"))

    def test_skip_node_modules(self):
        assert index_code._should_skip(Path("node_modules/foo/bar.js"))

    def test_skip_target(self):
        assert index_code._should_skip(Path("target/debug/build.rs"))

    def test_skip_pycache(self):
        assert index_code._should_skip(Path("src/__pycache__/mod.pyc"))

    def test_normal_path(self):
        assert not index_code._should_skip(Path("src/main.rs"))

    def test_nested_normal(self):
        assert not index_code._should_skip(Path("src/utils/helpers.py"))


class TestContentHash:
    def test_deterministic(self):
        h1 = index_code._content_hash("hello")
        h2 = index_code._content_hash("hello")
        assert h1 == h2

    def test_different_input(self):
        h1 = index_code._content_hash("hello")
        h2 = index_code._content_hash("world")
        assert h1 != h2

    def test_is_sha256_hex(self):
        h = index_code._content_hash("test")
        assert len(h) == 64
        int(h, 16)  # must be valid hex


class TestPointId:
    def test_deterministic(self):
        id1 = index_code._point_id("proj", "src/main.rs", "main", 1)
        id2 = index_code._point_id("proj", "src/main.rs", "main", 1)
        assert id1 == id2

    def test_different_inputs(self):
        id1 = index_code._point_id("proj", "src/main.rs", "main", 1)
        id2 = index_code._point_id("proj", "src/main.rs", "main", 2)
        assert id1 != id2

    def test_is_valid_uuid(self):
        import uuid

        pid = index_code._point_id("p", "f", "s", 1)
        uuid.UUID(pid)  # must not raise


class TestContextLine:
    def test_format(self):
        line = index_code._context_line("src/main.rs", "rust", "rust-web")
        assert "src/main.rs" in line
        assert "rust" in line
        assert "rust-web" in line


class TestExtractSymbols:
    def test_rust_function(self):
        code = b'fn main() { println!("hello"); }'
        symbols = index_code._extract_symbols(code, "rust", "main.rs")
        assert len(symbols) == 1
        assert symbols[0]["symbol_name"] == "main"
        assert symbols[0]["chunk_type"] == "function_item"
        assert symbols[0]["start_line"] == 1

    def test_rust_struct_and_impl(self):
        code = b"struct Foo { x: i32 }\nimpl Foo { fn bar(&self) {} }"
        symbols = index_code._extract_symbols(code, "rust", "foo.rs")
        types = {s["chunk_type"] for s in symbols}
        assert "struct_item" in types
        assert "impl_item" in types
        names = {s["symbol_name"] for s in symbols}
        assert "Foo" in names

    def test_python_class_and_function(self):
        code = b"class Foo:\n    pass\n\ndef bar():\n    pass\n"
        symbols = index_code._extract_symbols(code, "python", "mod.py")
        assert len(symbols) == 2
        names = {s["symbol_name"] for s in symbols}
        assert names == {"Foo", "bar"}

    def test_go_function_and_type(self):
        code = b"package main\nfunc Foo() {}\ntype Bar struct { X int }\n"
        symbols = index_code._extract_symbols(code, "go", "main.go")
        names = {s["symbol_name"] for s in symbols}
        assert "Foo" in names
        assert "Bar" in names

    def test_typescript_all_nodes(self):
        code = (
            b"function foo() {}\nclass Bar {}\ninterface IBaz {}\ntype Alias = string\n"
        )
        symbols = index_code._extract_symbols(code, "typescript", "mod.ts")
        assert len(symbols) == 4
        names = {s["symbol_name"] for s in symbols}
        assert names == {"foo", "Bar", "IBaz", "Alias"}

    def test_unknown_lang(self):
        symbols = index_code._extract_symbols(b"x = 1", "java", "Main.java")
        assert symbols == []

    def test_empty_source(self):
        symbols = index_code._extract_symbols(b"", "rust", "empty.rs")
        assert symbols == []


class TestChunkFallback:
    def test_single_chunk(self):
        source = "\n".join(f"line {i}" for i in range(50))
        chunks = index_code._chunk_fallback(source, "file.txt")
        assert len(chunks) == 1
        assert chunks[0]["chunk_type"] == "chunk"
        assert chunks[0]["start_line"] == 1
        assert chunks[0]["end_line"] == 50

    def test_multiple_chunks(self):
        source = "\n".join(f"line {i}" for i in range(250))
        chunks = index_code._chunk_fallback(source, "file.txt")
        assert len(chunks) == 3
        assert chunks[0]["start_line"] == 1
        assert chunks[0]["end_line"] == 100
        assert chunks[1]["start_line"] == 101
        assert chunks[2]["start_line"] == 201

    def test_empty_source(self):
        chunks = index_code._chunk_fallback("", "file.txt")
        assert chunks == []

    def test_whitespace_only_skipped(self):
        source = "   \n  \n   "
        chunks = index_code._chunk_fallback(source, "file.txt")
        assert chunks == []


class TestCollectFiles:
    def test_collects_supported_files(self, tmp_path):
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "main.rs").write_text("fn main() {}")
        (tmp_path / "src" / "lib.py").write_text("pass")
        (tmp_path / "README.md").write_text("# hi")
        files = index_code._collect_files(tmp_path)
        exts = {f.suffix for f in files}
        assert ".rs" in exts
        assert ".py" in exts
        assert ".md" not in exts

    def test_skips_excluded_dirs(self, tmp_path):
        (tmp_path / "node_modules" / "pkg").mkdir(parents=True)
        (tmp_path / "node_modules" / "pkg" / "index.ts").write_text("export {}")
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "app.ts").write_text("function f() {}")
        files = index_code._collect_files(tmp_path)
        paths = [str(f) for f in files]
        assert not any("node_modules" in p for p in paths)
        assert len(files) == 1
