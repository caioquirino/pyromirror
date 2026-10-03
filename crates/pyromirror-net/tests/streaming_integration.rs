use pyromirror_net::{FrameReceiver, FrameSender};
use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

#[test]
fn test_end_to_end_streaming_pipeline() {
    let sender_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed to bind sender socket");
    let receiver_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed to bind receiver socket");

    let receiver_addr = receiver_socket.local_addr().expect("Failed to get receiver addr");
    receiver_socket.set_read_timeout(Some(Duration::from_millis(200))).expect("Failed to set timeout");

    let mut sender = FrameSender::new(sender_socket, receiver_addr, 300, 1400); // 300 Mbps, 1400 MTU
    let mut receiver = FrameReceiver::new(4 * 1024 * 1024);

    let frame_pts = 1_000_000u64;
    let frame_payload = vec![0xABu8; 15_000]; // 15 KB frame spanning ~11 MTU packets

    // Send frame
    let sent_bytes = sender.send_frame(&frame_payload, frame_pts, true).expect("Failed to send frame");
    assert!(sent_bytes > frame_payload.len());

    // Receive packets and reassemble
    let mut buf = [0u8; 2048];
    let mut assembled = None;

    let handle = thread::spawn(move || {
        for _ in 0..50 {
            if let Ok((len, _)) = receiver_socket.recv_from(&mut buf) {
                if let Ok(Some((data, pts))) = receiver.push_datagram(&buf[..len]) {
                    assembled = Some((data, pts));
                    break;
                }
            }
        }
        assembled
    });

    let result = handle.join().expect("Receiver thread panicked");
    assert!(result.is_some(), "Frame failed to assemble within timeout");

    let (received_data, received_pts) = result.unwrap();
    assert_eq!(received_pts, frame_pts);
    assert_eq!(received_data.len(), frame_payload.len());
    assert_eq!(received_data, frame_payload);
}

#[test]
fn test_large_frame_assembly() {
    let sender_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed to bind sender socket");
    let receiver_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed to bind receiver socket");

    let receiver_addr = receiver_socket.local_addr().expect("Failed to get receiver addr");
    receiver_socket.set_read_timeout(Some(Duration::from_millis(500))).expect("Failed to set timeout");

    // 1000 Mbps so local test runs fast
    let mut sender = FrameSender::new(sender_socket, receiver_addr, 1000, 1400);
    let mut receiver = FrameReceiver::new(8 * 1024 * 1024);

    let frame_pts = 2_000_000u64;
    // 500 KB test payload
    let frame_payload = vec![0x33u8; 500_000];

    let handle = thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let mut assembled = None;
        while let Ok((len, _)) = receiver_socket.recv_from(&mut buf) {
            if let Ok(Some((data, pts))) = receiver.push_datagram(&buf[..len]) {
                assembled = Some((data, pts));
                break;
            }
        }
        assembled
    });

    sender.send_frame(&frame_payload, frame_pts, true).expect("Failed to send frame");

    let result = handle.join().expect("Receiver thread panicked");
    assert!(result.is_some(), "Large frame failed to assemble");
    let (data, pts) = result.unwrap();
    assert_eq!(pts, frame_pts);
    assert_eq!(data.len(), 500_000);
}

