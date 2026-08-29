//! The audio callback proven in isolation, with no decoder and no device.

use std::sync::Arc;

use dubplate_audio::device::Renderer;
use dubplate_audio::ring::{self, PlaybackShared};

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// 10ms at 48kHz, which is exactly the gain ramp length.
const RAMP_FRAMES: usize = 480;

fn setup() -> (rtrb::Producer<f32>, ring::RingRenderer, Arc<PlaybackShared>) {
    let shared = Arc::new(PlaybackShared::new());
    let (producer, renderer) = ring::open(RATE, CHANNELS, Arc::clone(&shared));
    (producer, renderer, shared)
}

fn push_frames(producer: &mut rtrb::Producer<f32>, frames: usize, value: f32) -> usize {
    let mut pushed = 0;
    for _ in 0..frames {
        for _ in 0..CHANNELS {
            if producer.push(value).is_err() {
                return pushed;
            }
        }
        pushed += 1;
    }
    pushed
}

fn buffer(frames: usize) -> Vec<f32> {
    vec![f32::NAN; frames * CHANNELS as usize]
}

#[test]
fn an_empty_ring_renders_silence_and_counts_an_underrun() {
    let (_producer, mut renderer, shared) = setup();
    shared.set_playing(true);

    let mut output = buffer(256);
    renderer.render(&mut output, CHANNELS);

    assert!(output.iter().all(|s| *s == 0.0), "must be silence, not garbage");
    assert_eq!(shared.underruns(), 1, "the decoder fell behind and we should know");
}

#[test]
fn a_dry_ring_while_paused_is_not_an_underrun() {
    let (_producer, mut renderer, shared) = setup();
    shared.set_playing(false);

    let mut output = buffer(256);
    renderer.render(&mut output, CHANNELS);

    assert!(output.iter().all(|s| *s == 0.0));
    assert_eq!(shared.underruns(), 0, "silence while paused is the point");
}

#[test]
fn queued_audio_comes_out_ramped_then_at_full_gain() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    shared.set_target_gain(1.0);
    push_frames(&mut producer, 2000, 1.0);

    let mut output = buffer(2000);
    renderer.render(&mut output, CHANNELS);

    // The first frame must not jump straight to full scale: that is the click.
    assert!(output[0] > 0.0 && output[0] < 0.01, "first frame {}", output[0]);
    // Ramp is monotonic while climbing.
    for pair in output.chunks(CHANNELS as usize).take(RAMP_FRAMES).collect::<Vec<_>>().windows(2) {
        assert!(pair[1][0] >= pair[0][0]);
    }
    // Past the ramp it is the sample value untouched.
    let settled = output[RAMP_FRAMES * CHANNELS as usize + 64];
    assert!((settled - 1.0).abs() < 1e-6, "settled at {settled}");
    assert_eq!(shared.underruns(), 0);
}

#[test]
fn both_channels_carry_their_own_sample() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    // Left = 1.0, right = -1.0, interleaved.
    for _ in 0..600 {
        producer.push(1.0).unwrap();
        producer.push(-1.0).unwrap();
    }

    let mut output = buffer(600);
    renderer.render(&mut output, CHANNELS);

    let left = output[RAMP_FRAMES * 2 + 10];
    let right = output[RAMP_FRAMES * 2 + 11];
    assert!(left > 0.9, "left {left}");
    assert!(right < -0.9, "right {right} -- channels must not be collapsed");
}

#[test]
fn a_seek_throws_away_everything_already_queued() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    push_frames(&mut producer, 1000, 1.0);
    let queued_before = producer.slots();

    // The control thread seeks to 5 seconds in.
    let generation = shared.begin_seek(5 * RATE as u64);

    let mut output = buffer(256);
    renderer.render(&mut output, CHANNELS);

    assert!(
        output.iter().all(|s| *s == 0.0),
        "pre-seek audio must never reach the device"
    );
    assert!(producer.slots() > queued_before, "the ring should have been drained");
    assert_eq!(
        shared.drained_generation(),
        generation,
        "the decoder waits on this before writing post-seek audio"
    );
}

#[test]
fn position_jumps_to_the_seek_target_not_past_it() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    push_frames(&mut producer, 1000, 1.0);

    let mut output = buffer(500);
    renderer.render(&mut output, CHANNELS);
    assert_eq!(shared.frames_played(), 500);

    shared.begin_seek(90 * RATE as u64);
    renderer.render(&mut output, CHANNELS);

    // Exactly the seek target: the stale frames already played must not be
    // added on top, or the seek bar lands in the wrong place.
    assert_eq!(shared.frames_played(), 90 * RATE as u64);
}

#[test]
fn a_faded_out_pause_leaves_the_ring_alone() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    push_frames(&mut producer, 4000, 1.0);

    // Play far enough to settle at full gain.
    let mut output = buffer(1000);
    renderer.render(&mut output, CHANNELS);

    // Pause, and let the fade finish.
    shared.set_playing(false);
    renderer.render(&mut output, CHANNELS);

    let queued_when_silent = producer.slots();
    renderer.render(&mut output, CHANNELS);
    renderer.render(&mut output, CHANNELS);

    assert_eq!(
        producer.slots(),
        queued_when_silent,
        "a paused player must not quietly eat the buffer it will resume from"
    );
    assert_eq!(shared.frames_played(), shared.frames_played());
}

#[test]
fn pausing_fades_rather_than_cutting() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    push_frames(&mut producer, 4000, 1.0);

    let mut settle = buffer(1000);
    renderer.render(&mut settle, CHANNELS);

    shared.set_playing(false);
    let mut output = buffer(RAMP_FRAMES);
    renderer.render(&mut output, CHANNELS);

    // Starts near full scale, ends at zero, and never jumps.
    assert!(output[0] > 0.9, "first frame after pause {}", output[0]);
    assert!(output[output.len() - 1].abs() < 1e-6, "should reach silence");
    for pair in output.chunks(CHANNELS as usize).collect::<Vec<_>>().windows(2) {
        assert!(pair[1][0] <= pair[0][0] + 1e-6, "fade must be monotonic");
    }
}

#[test]
fn a_partially_filled_ring_is_topped_up_with_silence() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    shared.set_target_gain(1.0);
    push_frames(&mut producer, 100, 1.0);

    let mut output = buffer(400);
    renderer.render(&mut output, CHANNELS);

    // 100 frames of audio, then silence rather than stale or uninitialised data.
    assert!(output[..100 * CHANNELS as usize].iter().all(|s| *s >= 0.0));
    assert!(
        output[100 * CHANNELS as usize..].iter().all(|s| *s == 0.0),
        "the tail must be silence"
    );
    assert_eq!(shared.underruns(), 1);
    assert_eq!(shared.frames_played(), 100);
}

#[test]
fn gain_of_zero_is_silence_without_stalling_playback() {
    let (mut producer, mut renderer, shared) = setup();
    shared.set_playing(true);
    shared.set_target_gain(0.0);
    push_frames(&mut producer, 1000, 1.0);

    let mut output = buffer(1000);
    renderer.render(&mut output, CHANNELS);

    assert!(output.iter().all(|s| s.abs() < 1e-6), "muted output");
    // Muted is not paused: the track keeps moving.
    assert_eq!(shared.frames_played(), 1000);
}
