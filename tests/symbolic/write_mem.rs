fn main() {
    use std::io::Read;

    let mut data: Vec<u8> = vec![0; 4];
    let mut stdin = ::std::io::stdin();
    stdin.read(&mut data[..]).unwrap();


    // should panic on [ 7, 3, 21, 21 ]

    if data.len() >= 4 {
        let mut v = vec![0; data.len()];
        v[0] += data[3];
        v[1] = v[0] + data[2];
        if v[0] == 21 && v[1] == 42 {
            v[0] = data[0] + data[1];
            v[3] = data[0];
            if v[3] == 7 && v[0] == 10 {
                panic!()
            }
        }
    }
}
