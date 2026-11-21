"""Section 1 TRAP tests: CSV parsing."""
from conftest import write_files

from csvlines import csv_files, parse_csv

COLS = ["a", "b", "c"]


def parse(con, tmp_path, files, cols=COLS):
    paths = write_files(str(tmp_path), files)
    parse_csv(con, "t", paths, cols)
    return sorted(con.execute("SELECT * FROM t").fetchall(), key=lambda r: tuple("" if v is None else v for v in r))


def test_cr_is_an_ordinary_character_not_a_line_ending(con, tmp_path):
    # TRAP: DuckDB's own reader splits `x\r` and `e3\r,..`; we must not.
    rows = parse(con, tmp_path, {"f.csv": "a,b,c\nx,y,1000\r\ne3\r,y,z\n"})
    assert rows == [("e3\r", "y", "z"), ("x", "y", "1000\r")]


def test_cr_in_the_middle_of_a_line_is_kept(con, tmp_path):
    assert parse(con, tmp_path, {"f.csv": "a,b,c\nx\ry,b,c\n"}) == [("x\ry", "b", "c")]


def test_spaces_are_part_of_the_value_quoted_or_not(con, tmp_path):
    rows = parse(con, tmp_path, {"f.csv": 'a,b,c\n u1,u1 ," u1"\n'})
    assert rows == [(" u1", "u1 ", " u1")]


def test_whole_field_quotes_are_stripped_and_empty_means_null(con, tmp_path):
    rows = parse(con, tmp_path, {"f.csv": 'a,b,c\n"u1",,""\n"",x,"y z"\n'})
    assert rows == [(None, "x", "y z"), ("u1", None, None)]


def test_bad_quote_placement_drops_the_whole_line(con, tmp_path):
    text = "a,b,c\n" + "\n".join([
        'u1,u5",c',      # quote at the end only
        '"u1,b,c',       # unbalanced
        'x"y",b,c',      # quote inside a field
        '"a"b,b,c',      # text after the closing quote
        '"a""b",b,c',    # doubled quote is not an escape here
        '",b,c',         # a lone quote
        'ok,ok,ok',
    ]) + "\n"
    assert parse(con, tmp_path, {"f.csv": text}) == [("ok", "ok", "ok")]


def test_wrong_field_count_is_dropped(con, tmp_path):
    text = "a,b,c\n" + "\n".join(["1,2", "1,2,3,4", "1,2,3", ",,"]) + "\n"
    assert parse(con, tmp_path, {"f.csv": text}) == [(None, None, None), ("1", "2", "3")]


def test_empty_lines_are_malformed_and_dropped(con, tmp_path):
    assert parse(con, tmp_path, {"f.csv": "a,b,c\n\n1,2,3\n\n\n"}) == [("1", "2", "3")]


def test_header_only_files_and_no_trailing_newline(con, tmp_path):
    rows = parse(con, tmp_path, {"h.csv": "a,b,c\n", "h2.csv": "a,b,c", "z.csv": "a,b,c\n1,2,3"})
    assert rows == [("1", "2", "3")]


def test_empty_file_is_fine(con, tmp_path):
    assert parse(con, tmp_path, {"e.csv": "", "z.csv": "a,b,c\n1,2,3\n"}) == [("1", "2", "3")]


def test_only_the_first_line_of_each_file_is_a_header(con, tmp_path):
    # A data row equal to the header is data (fixture L3 relies on this).
    text = "a,b,c\na,b,c\n1,2,3\n"
    assert parse(con, tmp_path, {"f.csv": text, "g.csv": text}) == [
        ("1", "2", "3"), ("1", "2", "3"), ("a", "b", "c"), ("a", "b", "c")]


def test_many_files_are_all_read_and_header_skipped_in_each(con, tmp_path):
    files = {f"part-{i:05d}.csv": f"a,b,c\n{i},x,y\n" for i in range(40)}
    rows = parse(con, tmp_path, files)
    assert len(rows) == 40 and {r[0] for r in rows} == {str(i) for i in range(40)}


def test_csv_files_reads_only_csv_files(tmp_path):
    write_files(str(tmp_path), {"b.csv": "h\n", "a.csv": "h\n", "notes.txt": "x", "c.csv.bak": "x"})
    assert [p.rsplit("/", 1)[1] for p in csv_files(str(tmp_path))] == ["a.csv", "b.csv"]
    assert csv_files(str(tmp_path / "missing")) == []


def test_no_files_gives_an_empty_table_with_the_columns(con, tmp_path):
    parse_csv(con, "t", [], COLS)
    assert con.execute("SELECT * FROM t").fetchall() == []
    assert [d[0] for d in con.description] == COLS


def test_unicode_and_non_ascii_values_survive_unchanged(con, tmp_path):
    rows = parse(con, tmp_path, {"f.csv": "a,b,c\nü,٢٥٠,２５０\n"})
    assert rows == [("ü", "٢٥٠", "２５０")]
