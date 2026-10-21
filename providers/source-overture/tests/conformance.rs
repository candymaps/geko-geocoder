use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use arrow_array::builder::{ListBuilder, MapBuilder, StringBuilder, StructBuilder};
use arrow_array::{Array, ArrayRef, Float64Array, RecordBatch, StringArray, StructArray};
use arrow_schema::{DataType, Field, Fields, Schema};
use geko_core::country::{CountryCode, RegionCode};
use geko_core::geo::{BBox, Coord};
use geko_core::regions::Regions;
use geko_core::ports::{DownloadOutcome, DownloadRequest, Downloader, RawDir, SourceProvider, StagingDir};
use geko_core::scope::{Partition, Scope};
use geko_core::{GekoError, Result};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use source_overture::{OvertureConfig, OvertureSource};

const RELEASE: &str = "2026-09-23.1";
const FILE: &str = "part-00000.zstd.parquet";

/// A place: id, name, other names, category, confidence, status, country, and point.
type Place = (&'static str, &'static str, Option<(&'static str, &'static str)>, &'static str, f64, Option<&'static str>, &'static str, (f64, f64));

/// Places in Overture's schema, two per row group: one group in the Balearics, one in Paris.
fn places_parquet(path: &Path) {
    let places: Vec<Place> = vec![
        ("p1", "Pizza Industria", None, "restaurant", 0.95, Some("open"), "ES", (2.6285, 39.5678)),
        ("p2", "Farma Vet Balear", Some(("en", "Farma Vet")), "animal_or_pet_service", 0.84, None, "ES", (2.6557, 39.5786)),
        ("p3", "Closed Bar", None, "bar", 0.95, Some("permanently_closed"), "ES", (2.65, 39.57)),
        ("p4", "Unsure Shop", None, "convenience_store", 0.3, Some("open"), "ES", (2.64, 39.56)),
        ("f1", "Café de Flore", None, "cafe", 0.99, Some("open"), "FR", (2.3325, 48.8541)),
        ("f2", "Musée", None, "museum", 0.99, Some("open"), "FR", (2.3376, 48.8606)),
    ];
    let str_field = |n: &str| Field::new(n, DataType::Utf8, true);
    let map_type = MapBuilder::new(None, StringBuilder::new(), StringBuilder::new()).finish();
    let names_fields = Fields::from(vec![str_field("primary"), Field::new("common", map_type.data_type().clone(), true)]);
    let address_fields = Fields::from(vec![str_field("country")]);
    let bbox_fields = Fields::from(["xmin", "xmax", "ymin", "ymax"].map(|n| Field::new(n, DataType::Float64, false)).to_vec());
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("names", DataType::Struct(names_fields.clone()), true),
        Field::new("confidence", DataType::Float64, true),
        str_field("operating_status"),
        str_field("basic_category"),
        Field::new("brand", DataType::Struct(Fields::from(vec![str_field("wikidata")])), true),
        Field::new("addresses", DataType::List(Arc::new(Field::new("element", DataType::Struct(address_fields.clone()), true))), true),
        Field::new("bbox", DataType::Struct(bbox_fields.clone()), true),
    ]));
    let props = WriterProperties::builder().set_max_row_group_size(4).build();
    let mut writer = ArrowWriter::try_new(std::fs::File::create(path).unwrap(), Arc::clone(&schema), Some(props)).unwrap();
    for group in [&places[..4], &places[4..]] {
        let mut names = StructBuilder::new(names_fields.clone(), vec![Box::new(StringBuilder::new()), Box::new(MapBuilder::new(None, StringBuilder::new(), StringBuilder::new()))]);
        let mut addresses = ListBuilder::new(StructBuilder::new(address_fields.clone(), vec![Box::new(StringBuilder::new())])).with_field(Field::new("element", DataType::Struct(address_fields.clone()), true));
        for p in group {
            names.field_builder::<StringBuilder>(0).unwrap().append_value(p.1);
            let common = names.field_builder::<MapBuilder<StringBuilder, StringBuilder>>(1).unwrap();
            match p.2 {
                Some((k, v)) => {
                    common.keys().append_value(k);
                    common.values().append_value(v);
                    common.append(true).unwrap();
                }
                None => common.append(false).unwrap(),
            }
            names.append(true);
            addresses.values().field_builder::<StringBuilder>(0).unwrap().append_value(p.6);
            addresses.values().append(true);
            addresses.append(true);
        }
        let coords = |f: fn(&Place) -> f64| Arc::new(Float64Array::from(group.iter().map(f).collect::<Vec<_>>())) as ArrayRef;
        let bbox = StructArray::new(bbox_fields.clone(), vec![coords(|p| p.7.0), coords(|p| p.7.0), coords(|p| p.7.1), coords(|p| p.7.1)], None);
        let brand = StructArray::new(Fields::from(vec![str_field("wikidata")]), vec![Arc::new(StringArray::from(vec![None::<&str>; group.len()])) as ArrayRef], None);
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(group.iter().map(|p| p.0).collect::<Vec<_>>())),
            Arc::new(names.finish()),
            Arc::new(Float64Array::from(group.iter().map(|p| p.4).collect::<Vec<_>>())),
            Arc::new(StringArray::from(group.iter().map(|p| p.5).collect::<Vec<_>>())),
            Arc::new(StringArray::from(group.iter().map(|p| p.3).collect::<Vec<_>>())),
            Arc::new(brand),
            Arc::new(addresses.finish()),
            Arc::new(bbox),
        ];
        writer.write(&RecordBatch::try_new(Arc::clone(&schema), columns).unwrap()).unwrap();
        writer.flush().unwrap();
    }
    writer.close().unwrap();
}

/// The bucket, served from a local file: the listing, and byte ranges of the places file.
struct Bucket {
    file: Vec<u8>,
    ranges: Mutex<Vec<(u64, u64)>>,
}

impl Downloader for Bucket {
    fn fetch(&self, req: &DownloadRequest, dest: &Path) -> Result<DownloadOutcome> {
        let xml = if req.url.contains("delimiter") {
            format!("<ListBucketResult><CommonPrefixes><Prefix>release/{RELEASE}/</Prefix></CommonPrefixes></ListBucketResult>")
        } else {
            format!(
                "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>release/{RELEASE}/theme=places/type=place/{FILE}</Key><Size>{}</Size></Contents></ListBucketResult>",
                self.file.len()
            )
        };
        std::fs::write(dest, &xml)?;
        Ok(DownloadOutcome { downloaded: true, sha256: String::new(), size: xml.len() as u64 })
    }

    fn fetch_range(&self, _url: &str, start: u64, len: u64) -> Result<Vec<u8>> {
        self.ranges.lock().unwrap().push((start, len));
        self.file.get(start as usize..(start + len) as usize).map(<[u8]>::to_vec).ok_or_else(|| GekoError::Data("range past the end".into()))
    }
}

fn spain() -> Partition {
    Partition { country: CountryCode::new("ES").unwrap(), region: None }
}

#[test]
fn fetches_only_the_row_groups_of_the_scope_and_stages_them() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.parquet");
    places_parquet(&remote);
    let bucket = Bucket { file: std::fs::read(&remote).unwrap(), ranges: Mutex::new(Vec::new()) };
    let source = OvertureSource::new(OvertureConfig { base: "mem://bucket".into(), ..Default::default() }, Default::default());
    let raw = RawDir { path: tmp.path().join("raw") };
    let old = raw.path.join("2020-01-01.0");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join(FILE), b"old").unwrap();
    let mut scope = Scope::default();
    scope.add(CountryCode::new("ES").unwrap(), vec![]).unwrap();

    source.fetch(&scope, &bucket, &raw).unwrap();
    assert!(!old.exists(), "an older release is removed");
    let fetched: u64 = bucket.ranges.lock().unwrap().iter().map(|r| r.1).sum();
    assert!(fetched < bucket.file.len() as u64, "the row group of Paris is not downloaded: {fetched} of {}", bucket.file.len());
    let ranges = bucket.ranges.lock().unwrap().len();
    let times = |dir: &Path| -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
        let mut out = Vec::new();
        for e in walk(dir) {
            out.push((e.clone(), std::fs::metadata(&e).unwrap().modified().unwrap()));
        }
        out.sort();
        out
    };
    let before = times(&raw.path.join(RELEASE));
    let pointer = std::fs::metadata(raw.path.join("release")).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    source.fetch(&scope, &bucket, &raw).unwrap();
    assert_eq!(bucket.ranges.lock().unwrap().len(), ranges, "a second fetch downloads nothing");
    assert_eq!(std::fs::metadata(raw.path.join("release")).unwrap().modified().unwrap(), pointer, "nor rewrites the release");
    assert_eq!(times(&raw.path.join(RELEASE)).into_iter().filter(|(p, _)| p.extension().is_some_and(|x| x == "parquet" || x == "json")).collect::<Vec<_>>(),
        before.into_iter().filter(|(p, _)| p.extension().is_some_and(|x| x == "parquet" || x == "json")).collect::<Vec<_>>(), "nor the copies");

    let out = StagingDir { path: tmp.path().join("staged") };
    let stats = source.stage(&spain(), &raw, &out).unwrap();
    assert_eq!(stats.records, 2, "open, sure and in Spain: Pizza Industria and Farma Vet Balear");
    let file = std::fs::File::open(out.path.join("pois.parquet")).unwrap();
    let batch = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap().next().unwrap().unwrap();
    let col = |n: &str| batch.column_by_name(n).unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
    assert_eq!(col("category").value(0), "amenity=restaurant");
    assert_eq!(col("category").value(1), "amenity=veterinary");
    assert_eq!(col("names").value(1), r#"{"name:en":"Farma Vet"}"#);
}

#[test]
fn a_partition_with_a_box_fetches_only_what_meets_it() {
    let es = CountryCode::new("ES").unwrap();
    let region = |code: &str, (lat0, lon0, lat1, lon1): (f64, f64, f64, f64)| geko_core::regions::Region {
        code: RegionCode::new(code).unwrap(),
        name: code.into(),
        parent: None,
        attributes: Default::default(),
        bbox: Some(BBox::new(Coord::from_degrees(lat0, lon0).unwrap(), Coord::from_degrees(lat1, lon1).unwrap()).unwrap()),
    };
    let regions = Regions::new(es, vec![region("ES-PM", (38.6, 1.1, 40.1, 4.4)), region("ES-M", (39.8, -4.6, 41.2, -3.0))]).unwrap();
    // Bytes of row groups downloaded, beyond the footer (the first two ranges).
    let fetched = |code: &str| {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.parquet");
        places_parquet(&remote);
        let bucket = Bucket { file: std::fs::read(&remote).unwrap(), ranges: Mutex::new(Vec::new()) };
        let config = OvertureConfig { base: "mem://bucket".into(), ..Default::default() };
        let source = OvertureSource::new(config, BTreeMap::from([(es, regions.clone())]));
        let mut scope = Scope::default();
        scope.add(es, vec![RegionCode::new(code).unwrap()]).unwrap();
        source.fetch(&scope, &bucket, &RawDir { path: tmp.path().join("raw") }).unwrap();
        bucket.ranges.lock().unwrap().iter().skip(2).map(|r| r.1).sum::<u64>()
    };
    assert!(fetched("ES-PM") > 0, "the row group of the Balearic Islands meets their box");
    assert_eq!(fetched("ES-M"), 0, "no row group meets the box of Madrid, though both are in the box of Spain");
}

/// The bucket, bringing back removed copies (`Downloader::keeps_removed`).
struct Restoring<'a>(&'a Bucket);

impl Downloader for Restoring<'_> {
    fn fetch(&self, req: &DownloadRequest, dest: &Path) -> Result<DownloadOutcome> {
        self.0.fetch(req, dest)
    }
    fn fetch_range(&self, url: &str, start: u64, len: u64) -> Result<Vec<u8>> {
        self.0.fetch_range(url, start, len)
    }
    fn keeps_removed(&self) -> bool {
        false
    }
}

#[test]
fn a_removed_copy_keeps_its_identity_until_a_staging_needs_it() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.parquet");
    places_parquet(&remote);
    let bucket = Bucket { file: std::fs::read(&remote).unwrap(), ranges: Mutex::new(Vec::new()) };
    let source = OvertureSource::new(OvertureConfig { base: "mem://bucket".into(), ..Default::default() }, Default::default());
    let raw = RawDir { path: tmp.path().join("raw") };
    let mut scope = Scope::default();
    scope.add(CountryCode::new("ES").unwrap(), vec![]).unwrap();

    let identities = source.fetch(&scope, &bucket, &raw).unwrap().identities;
    let removable = source.removable(&raw).unwrap();
    assert_eq!(removable.len(), 1);
    std::fs::remove_file(&removable[0]).unwrap();
    let ranges = bucket.ranges.lock().unwrap().len();
    assert_eq!(source.fetch(&scope, &bucket, &raw).unwrap().identities, identities, "the same identity without the copy");
    assert_eq!(bucket.ranges.lock().unwrap().len(), ranges, "nothing downloaded");
    let out = StagingDir { path: tmp.path().join("staged") };
    let error = source.stage(&spain(), &raw, &out).err().unwrap().to_string();
    assert!(error.contains("keep_downloads"), "{error}");

    assert_eq!(source.fetch(&scope, &Restoring(&bucket), &raw).unwrap().identities, identities, "the same row groups");
    assert!(bucket.ranges.lock().unwrap().len() > ranges, "brought back");
    assert_eq!(source.stage(&spain(), &raw, &out).unwrap().records, 2);
}

#[test]
fn conformance() {
    let source = OvertureSource::new(OvertureConfig::default(), Default::default());
    geko_conformance::source::check_metadata(&source, CountryCode::new("ES").unwrap());

    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.parquet");
    places_parquet(&remote);
    let bucket = Bucket { file: std::fs::read(&remote).unwrap(), ranges: Mutex::new(Vec::new()) };
    let source = OvertureSource::new(OvertureConfig { base: "mem://bucket".into(), ..Default::default() }, Default::default());
    let raw = RawDir { path: tmp.path().join("overture") };
    let mut scope = Scope::default();
    scope.add(CountryCode::new("ES").unwrap(), vec![]).unwrap();
    source.fetch(&scope, &bucket, &raw).unwrap();
    geko_conformance::source::check_stage(&source, &spain(), &raw);
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}
