use pyromirror_net::{create_streaming_socket, packet_boundary, parse_video_datagram, FrameSender};
use std::time::Duration;

#[test]
fn frames_arrive_as_one_datagram_per_packet() {
    let sender_socket = create_streaming_socket("127.0.0.1:0".parse().unwrap(), 1 << 20).unwrap();
    let receiver_socket = create_streaming_socket("127.0.0.1:0".parse().unwrap(), 1 << 20).unwrap();
    receiver_socket.set_read_timeout(Some(Duration::from_millis(500))).unwrap();

    let mtu = 1400;
    let boundary = packet_boundary(mtu);
    let mut sender = FrameSender::new(sender_socket, receiver_socket.local_addr().unwrap(), 300);

    // Two frames of unevenly sized "codec packets".
    let frames: Vec<Vec<Vec<u8>>> = (0..2u8)
        .map(|f| (0..11u8).map(|i| vec![f * 16 + i; boundary - i as usize * 7]).collect())
        .collect();

    for (frame_index, frame) in frames.iter().enumerate() {
        let pts = 1_000_000 + frame_index as u64;
        let sent = sender.send_frame(frame.iter().map(|p| p.as_slice()), pts).unwrap();
        assert!(sent > frame.iter().map(|p| p.len()).sum::<usize>());

        let mut buf = [0u8; 2048];
        for (index, expected) in frame.iter().enumerate() {
            let (len, _) = receiver_socket.recv_from(&mut buf).expect("datagram lost on loopback");
            assert!(len <= mtu);
            let packet = parse_video_datagram(&buf[..len]).unwrap();
            assert_eq!(packet.frame_seq, frame_index as u32);
            assert_eq!(packet.index, index as u32);
            assert_eq!(packet.pts, pts);
            assert_eq!(packet.first, index == 0);
            assert_eq!(packet.last, index == frame.len() - 1);
            assert_eq!(packet.payload, expected.as_slice());
        }
    }
}
