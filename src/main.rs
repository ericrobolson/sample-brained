use std::collections::HashMap;
use std::fs::File;
use std::thread::sleep;

use rodio::buffer::SamplesBuffer;
use rodio::Sink;
use rodio::{source::Source, Decoder, OutputStream};
use std::io::{BufReader, BufWriter};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Options {
    block_divisor: usize,
    key_size: usize,
    mix: f32,
    save_file: bool,
    num_buckets: usize,
    use_target: bool,
}
impl Options {
    pub fn new() -> Self {
        Options {
            num_buckets: 4,
            mix: 1.0,
            save_file: false,
            block_divisor: 8,
            key_size: 16,
            use_target: true,
        }
    }
}

fn get_options() -> Options {
    match File::open("config.json") {
        Ok(_) => {
            let file = File::open("config.json").unwrap();
            let reader = BufReader::new(file);
            match serde_json::from_reader(reader) {
                Ok(options) => return options,
                Err(_) => return Options::new(),
            }
        }
        Err(_) => {
            let options = Options::new();
            let file = File::create("config.json").unwrap();
            let writer = BufWriter::new(file);
            serde_json::to_writer(writer, &options).unwrap();
            return Options::new();
        }
    }
}

fn main() {
    loop {
        run_audio();
    }
}

fn get_target() -> Decoder<BufReader<File>> {
    // Load a sound from a file, using a path relative to Cargo.toml
    let mut target_file = None;
    for entry in WalkDir::new("source") {
        if entry.is_err() {
            continue;
        }
        let path = entry.as_ref().unwrap().path();
        let ext = path.extension().unwrap_or_default().to_str().unwrap();
        if ext == "wav" {
            target_file = Some(path.to_str().unwrap().to_string());
            break;
        }
    }

    // Get first file in source/ directory
    let target_file = target_file.unwrap();

    let target = BufReader::new(File::open(target_file).unwrap());

    // Decode that sound file into a source
    let target_source = Decoder::new(target).unwrap();
    target_source
}

fn load_brain(block_len: usize, key_size: usize, options: &Options) -> Brain {
    let mut brain = Brain::new();
    for entry in WalkDir::new("brains") {
        if entry.is_err() {
            continue;
        }
        let path = entry.as_ref().unwrap().path();
        let ext = path.extension().unwrap_or_default().to_str().unwrap();
        if ext == "wav" {
            let file = BufReader::new(File::open(path).unwrap());
            let brain_source = Decoder::new(file).unwrap();
            let brain_data: Vec<f32> = brain_source.convert_samples().collect();
            let hashed_brain =
                HashedData::hash(block_len, key_size, options.num_buckets, &brain_data);
            brain.add_collection(hashed_brain);
        }
    }
    brain
}

fn run_audio() {
    let options = get_options();

    let mut manager = AudioManager::new();
    let target_source = get_target();

    // Now take that source, convert it to a vec, and play it
    {
        let channels = target_source.channels();
        let sample_rate = target_source.sample_rate();

        let sample_data = get_audio_data(target_source, &options);

        // Save data
        if options.save_file {
            let time_stamp = chrono::Local::now().format("%Y%m-%d_%H-%M-%S");
            let path = format!("output/{}.wav", time_stamp);
            std::fs::create_dir_all("output").unwrap();
            println!("Saving data to: {}", format!("output/{}.wav", time_stamp));
            save_data(&sample_data, channels, sample_rate, &path);
        }

        // Play data
        manager.play(sample_data.clone(), channels, sample_rate);
    }
}

fn get_audio_data(target_source: Decoder<BufReader<File>>, options: &Options) -> Vec<f32> {
    let time = target_source.total_duration().unwrap();

    let samples = target_source.convert_samples();
    let data: Vec<f32> = samples.collect();

    let ratio = 1.0 / time.as_secs_f32();
    let block_len = (data.len() as f32 * ratio) as usize;
    let block_divisor = options.block_divisor;
    let block_len = block_len / block_divisor.max(1);
    const MAX_KEY_VALUES: usize = 128;
    let key_size = options.key_size.min(MAX_KEY_VALUES);
    let hashed_data = HashedData::hash(block_len, key_size, options.num_buckets, &data);

    let mut sample_data: Vec<f32> = Vec::with_capacity(data.len());
    let mut brain = load_brain(block_len, key_size, &options);
    for (key, block) in hashed_data.get_ordered_data() {
        {
            if let Some(brain_block) = brain.get_block(key.clone()) {
                let mix = options.mix;
                let mut new_block = Vec::with_capacity(block.len());
                for i in 0..block.len() {
                    let mut value = brain_block[i];

                    if options.use_target {
                        value = value * (1.0 - mix) + block[i] * mix;
                    }

                    new_block.push(value);
                }

                // Push brain data
                sample_data.extend(new_block);
            } else {
                // Push target data
                let push_empty = options.use_target == false;
                if push_empty {
                    let empty = vec![0.0; block.len()];
                    sample_data.extend(empty);
                } else {
                    sample_data.extend(block);
                }
            }
        }
    }
    sample_data
}

pub struct AudioManager {
    sink: Sink,
    stream: OutputStream,
    stream_handle: rodio::OutputStreamHandle,
}
impl AudioManager {
    pub fn new() -> Self {
        let (stream, stream_handle) = OutputStream::try_default().unwrap();
        let sink = Sink::try_new(&stream_handle).unwrap();

        AudioManager {
            sink,
            stream,
            stream_handle,
        }
    }

    pub fn play(&mut self, data: Vec<f32>, channels: u16, sample_rate: u32) {
        let samples_buffer = SamplesBuffer::new(channels, sample_rate, data);
        self.sink.append(samples_buffer);
        while !self.sink.empty() {
            sleep(std::time::Duration::from_millis(100));
        }
    }
}
impl Drop for AudioManager {
    fn drop(&mut self) {
        self.sink.stop();
    }
}

type Buff = Vec<f32>;
type Key = String;

struct Brain {
    collections: Vec<HashedData>,
}
impl Brain {
    pub fn new() -> Self {
        Brain {
            collections: Vec::new(),
        }
    }

    pub fn add_collection(&mut self, data: HashedData) {
        self.collections.push(data);
    }

    pub fn get_block(&mut self, key: Key) -> Option<Buff> {
        // Rotate collection order
        if self.collections.is_empty() == false {
            self.collections.rotate_left(1);
        }
        for collection in &self.collections {
            if let Some(block) = collection.get_block(key.clone()) {
                return Some(block);
            }
        }
        None
    }
}

fn save_data(data: &Vec<f32>, channels: u16, sample_rate: u32, file: &str) {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32, // bits in a float
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(file, spec).unwrap();
    for sample in data {
        writer.write_sample(*sample).unwrap();
    }
}

#[derive(Clone)]
struct HashedData {
    block_size: usize,
    key_len: usize,
    data: Vec<f32>,
    hashed_data: HashMap<Key, Buff>,
    ordered_data: Vec<(Key, Buff)>,
}

impl HashedData {
    fn get_block(&self, key: Key) -> Option<Buff> {
        self.hashed_data.get(&key).map(|v| v.clone())
    }

    fn get_ordered_data(&self) -> Vec<(Key, Buff)> {
        self.ordered_data.clone()
    }

    fn hash(block_size: usize, key_size: usize, num_buckets: usize, data: &Vec<f32>) -> Self {
        let mut s = Self {
            block_size,
            key_len: key_size,
            data: data.clone(),
            hashed_data: HashMap::new(),
            ordered_data: Vec::new(),
        };

        let mut padded_data = data.clone();
        while padded_data.len() % block_size != 0 {
            if padded_data.is_empty() {
                padded_data.push(0.0);
            } else {
                padded_data.push(padded_data[padded_data.len() - 1]);
            }
        }

        let num_blocks = padded_data.len() / block_size;
        let chars = vec![
            'a', 'b', 'c', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r',
            's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
        ];
        let num_buckets = num_buckets.max(1);
        let chars: Vec<char> = chars
            .iter()
            .map(|c| c.clone())
            .take(num_buckets.min(chars.len()))
            .collect();

        for block_idx in 0..num_blocks {
            let mut block = Vec::with_capacity(block_size);
            for i in 0..block_size {
                block.push(padded_data[block_idx * block_size + i]);
            }
            let (min_value, max_value) = get_min_max(&block);

            // Build key
            let mut key: Key = String::new();
            for k in 0..key_size {
                let iterations = block_size / key_size;
                // Take avg of elements in block
                let mut sum = 0.0;
                for l in 0..iterations {
                    let value = block[k * iterations + l];
                    let value = normalized_value(value, min_value, max_value);
                    sum += value;
                }

                let avg = sum / iterations as f32;

                let char_idx = (avg * chars.len() as f32) as usize;
                key.push(chars[char_idx.min(chars.len() - 1)]);
            }

            // Store block
            if !s.hashed_data.contains_key(&key) {
                s.hashed_data.insert(key.clone(), block.clone());
            }
            s.ordered_data.push((key, block));
        }

        s
    }
}

/// Normalize a value between 0 and 1
fn normalized_value(x: f32, min: f32, max: f32) -> f32 {
    (x - min) / (max - min)
}

fn get_min_max(data: &Vec<f32>) -> (f32, f32) {
    let max_value = {
        let mut max = 0.0;
        for i in 0..data.len() {
            if data[i] > max {
                max = data[i];
            }
        }
        max
    };
    let min_value = {
        let mut min = 0.0;
        for i in 0..data.len() {
            if data[i] < min {
                min = data[i];
            }
        }
        min
    };

    (min_value, max_value)
}
