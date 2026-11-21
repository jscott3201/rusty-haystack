// Independent finite oracle from pinned serde_json 1.0.151 tests/test.rs,
// test_roundtrip_f64 (lines 963-976). Decimal fixtures are compiled by Rust;
// hexadecimal IEEE-754 values pin the originals independently of our codecs.
pub const FINITE_NUMBERS: [(f64, u64); 10] = [
    (51.248178375505404, 0x4049_9fc4_4f1b_2f60),
    (-93.31137037688033, 0xc057_53ed_7e04_693b),
    (-36.573994842753436, 0xc042_4978_a9ba_d96e),
    (52.314008204106244, 0x404a_2831_6bbb_a7f0),
    (97.45365320034685, 0x4058_5d08_a76e_cdca),
    (2.0030397744267762e-253, 0x0b77_7f24_5544_ff83),
    (7.101215824554616e260, 0x7617_17c1_3ccb_d1b8),
    (1.769268377902049e74, 0x4f59_08c5_4991_eaa1),
    (-1.6727517818542075e58, 0xcc05_519b_7a80_b106),
    (3.9287532173373315e299, 0x7e22_c5d6_33b2_e702),
];
