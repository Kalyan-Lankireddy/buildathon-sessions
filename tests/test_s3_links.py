"""Section 3 TRAP tests: linked IDs."""
import random

from conftest import write_files

from links import build_person_map

HEADER = "user_id_a,user_id_b\n"


def person_map(con, tmp_path, files):
    paths = write_files(str(tmp_path), {k: HEADER + v for k, v in files.items()})
    build_person_map(con, paths)
    return dict(con.execute("SELECT id, person FROM person_map").fetchall())


def test_chains_join_across_lines_files_and_direction(con, tmp_path):
    m = person_map(con, tmp_path, {"a.csv": "u3,u9\nu9,u2\n", "b.csv": "u7,u9\n"})
    assert m == {"u2": "u2", "u3": "u2", "u7": "u2", "u9": "u2"}


def test_name_is_the_smallest_id_even_if_it_never_appears_in_an_event(con, tmp_path):
    # TRAP: the smallest ID can be one that no event or click uses; there is no event data here at all.
    m = person_map(con, tmp_path, {"a.csv": "u5,u1\nu1,u3\n"})
    assert set(m.values()) == {"u1"}


def test_upper_case_sorts_before_lower_case_and_becomes_the_name(con, tmp_path):
    # TRAP: byte order, not locale order. 'U7' < 'u1'.
    m = person_map(con, tmp_path, {"a.csv": "U7,u9\nu9,u1\n"})
    assert set(m.values()) == {"U7"}


def test_case_variants_are_different_ids(con, tmp_path):
    m = person_map(con, tmp_path, {"a.csv": "u1,u2\nU1,U2\n"})
    assert m == {"u1": "u1", "u2": "u1", "U1": "U1", "U2": "U1"}


def test_byte_order_beyond_ascii(con, tmp_path):
    # Byte-wise UTF-8 order: 'z' < 'é' (0xC3 0xA9) < full-width A (0xEF 0xBC 0xA1); digits < letters.
    m = person_map(con, tmp_path, {"a.csv": "é,z\nＡ,é\n9,a\n"})
    assert m["é"] == "z" and m["Ａ"] == "z" and m["a"] == "9"


def test_self_links_and_repeats_change_nothing(con, tmp_path):
    m = person_map(con, tmp_path, {"a.csv": "u10,u10\nu1,u2\nu2,u1\nu1,u2\nu1,u2\n"})
    assert "u10" not in m  # a self-link alone leaves the ID a person on its own (no mapping needed)
    assert m == {"u1": "u1", "u2": "u1"}


def test_malformed_and_empty_side_lines_are_dropped(con, tmp_path):
    text = "\n".join(["u1,u2,u3", "u1,", ",u3", 'u1,u5"', "", "u1", '"u4","u1"', "u4,u1", "u6,u6"]) + "\n"
    m = person_map(con, tmp_path, {"a.csv": text})
    # u1-u4 joined by the two good lines; nothing else joined to anything.
    assert m == {"u1": "u1", "u4": "u1"}


def test_a_data_row_equal_to_the_header_is_data(con, tmp_path):
    # Fixture L3: only the first line of a file is a header. 'A<tab>B' is the smallest ID of the lot.
    text = 'u4,u1\nuser_id_a,user_id_b\nu4,user_id_b\n"A\tB",user_id_a\n'
    m = person_map(con, tmp_path, {"a.csv": text})
    assert set(m.values()) == {"A\tB"} and set(m) == {"u4", "u1", "user_id_a", "user_id_b", "A\tB"}


def test_spaces_make_ids_distinct(con, tmp_path):
    m = person_map(con, tmp_path, {"a.csv": 'u1, u1\n"u1 ",u2\n'})
    # " u1" < "u1" ('space' is byte 0x20), so the first pair is named " u1"; "u1 " < "u2" names the second.
    assert m == {" u1": " u1", "u1": " u1", "u1 ": "u1 ", "u2": "u1 "}


def test_no_links_at_all(con, tmp_path):
    assert person_map(con, tmp_path, {"a.csv": "", "b.csv": ""}) == {}
    assert con.execute("SELECT count(*) FROM person_map").fetchone() == (0,)


def test_long_chain_in_random_order(con, tmp_path):
    # TRAP: chains hundreds of links long, drawn in random order.
    rng = random.Random(1)
    ids = [f"id{i:04d}" for i in range(400)]
    order = ids[:]
    rng.shuffle(order)
    lines = [f"{order[i]},{order[i + 1]}\n" for i in range(len(order) - 1)]
    rng.shuffle(lines)
    m = person_map(con, tmp_path, {"a.csv": "".join(lines)})
    assert len(m) == 400 and set(m.values()) == {"id0000"}


def test_shared_device_joins_many_people(con, tmp_path):
    # TRAP: one device ID linking thousands of otherwise separate people.
    lines = "".join(f"p{i:05d},dev1\n" for i in range(3000))
    m = person_map(con, tmp_path, {"a.csv": lines})
    assert len(m) == 3001 and set(m.values()) == {"dev1"}  # 'd' < 'p'


def test_random_graphs_match_a_plain_union_find(con, tmp_path):
    rng = random.Random(3)
    for trial in range(5):
        ids = [rng.choice(["u", "U", "a", "é", "Z"]) + str(rng.randrange(60)) for _ in range(80)]
        pairs = [(rng.choice(ids), rng.choice(ids)) for _ in range(50)]
        parent = {}

        def find(x):
            parent.setdefault(x, x)
            while parent[x] != x:
                parent[x] = parent[parent[x]]
                x = parent[x]
            return x

        for a, b in pairs:
            if a != b:
                parent[find(a)] = find(b)
        groups = {}
        for x in {i for p in pairs for i in p if p[0] != p[1]}:
            groups.setdefault(find(x), []).append(x)
        expected = {x: min(g, key=lambda s: s.encode("utf-8")) for g in groups.values() for x in g}
        got = person_map(con, tmp_path / f"t{trial}", {"a.csv": "".join(f"{a},{b}\n" for a, b in pairs)})
        assert got == expected
