extern crate cpal;
extern crate midir;
extern crate vst;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vst::event::MidiEvent;

use vst::buffer::SendEventBuffer;
use vst::host::HostBuffer;
use vst::plugin::Plugin;

use cpal::traits::{DeviceTrait, EventLoopTrait, HostTrait};
use cpal::{StreamData, UnknownTypeOutputBuffer};
use midir::MidiInput;
use vst::host::{Host, PluginLoader};

const BUFFER_SIZE: usize = 256;

struct MidiHost;

impl Host for MidiHost {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = if let Some(path) = std::env::args().nth(1) {
        PathBuf::from(path)
    } else {
        println!("Usage: midi_host <vst plugin>");
        println!("Example: midi_host target/release/examples/libsine_synth.so");
        std::process::exit(1);
    };

    let midi_input = MidiInput::new("midir").unwrap();
    let ports = midi_input.ports();
    for port in &ports {
        let name = midi_input.port_name(&port).unwrap();
        println!("{:?}", name);
    }
    let midi_port = &ports[1];

    // cpal needs to be initialized first as it otherwise panics with
    // Os { code: -2147417850, kind: Other, message: "Cannot change thread mode after it is set." }
    let host = cpal::default_host();
    let event_loop = cpal::default_host().event_loop();
    let device = host.default_output_device().expect("no output device available");
    let format = device.default_output_format().expect("error querying default format");
    println!("Output format: {:?}", format);
    let stream_id = event_loop.build_output_stream(&device, &format)?;

    let host = Arc::new(Mutex::new(MidiHost));
    let mut loader = PluginLoader::load(&path, host)?;
    let mut plugin = loader.instance()?;
    let info = plugin.get_info();
    println!("{:#?}", info);

    plugin.init();
    plugin.set_sample_rate(format.sample_rate.0 as f32);
    plugin.resume();

    event_loop.play_stream(stream_id)?;

    let (audio_tx, audio_rx) = std::sync::mpsc::sync_channel(BUFFER_SIZE);
    std::thread::spawn(move || {
        event_loop.run(move |_stream_id, stream_result| {
            if let StreamData::Output { buffer } = stream_result.expect("stream error") {
                match buffer {
                    UnknownTypeOutputBuffer::U16(mut buffer) => {
                        for dst in buffer.iter_mut() {
                            *dst = (audio_rx.recv().unwrap() + 1.0 * (std::u16::MAX / 2) as f32) as u16;
                        }
                    }
                    UnknownTypeOutputBuffer::I16(mut buffer) => {
                        for dst in buffer.iter_mut() {
                            *dst = (audio_rx.recv().unwrap() * std::i16::MAX as f32) as i16;
                        }
                    }
                    UnknownTypeOutputBuffer::F32(mut buffer) => {
                        for dst in buffer.iter_mut() {
                            *dst = audio_rx.recv().unwrap();
                        }
                    }
                }
            }
        });
    });
    let mut host_buffer: HostBuffer<f32> = HostBuffer::from_info(&info);
    let inputs = vec![vec![0.0; BUFFER_SIZE]; host_buffer.input_count()];
    let mut outputs = vec![vec![0.0; BUFFER_SIZE]; host_buffer.output_count()];
    let mut audio_buffer = host_buffer.bind(&inputs, &mut outputs);

    let (midi_tx, midi_rx) = std::sync::mpsc::channel();
    // MIDI connection must be kept alive until the end
    let _connection = midi_input
        .connect(
            midi_port,
            "midir-in",
            move |_, message, _| {
                println!("{:?}", message);
                midi_tx.send([message[0], message[1], message[2]]).unwrap();
            },
            (),
        )
        .unwrap();

    let mut send_buffer = SendEventBuffer::new(1);

    // FIXME: Channels...
    loop {
        plugin.process(&mut audio_buffer);

        for midi_msg in midi_rx.try_iter() {
            send_buffer.send_events_to_plugin(
                &[MidiEvent {
                    data: midi_msg, // Note on
                    delta_frames: 0,
                    live: true,
                    note_length: None,
                    note_offset: None,
                    detune: 0,
                    note_off_velocity: 0,
                }],
                &mut plugin,
            );
        }

        for s in outputs[0].iter().zip(outputs[1].iter()) {
            audio_tx.send(*s.0).unwrap();
            audio_tx.send(*s.1).unwrap();
        }
    }
}
