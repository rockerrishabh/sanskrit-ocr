use actix_files as fs;
use actix_multipart::Multipart;
use actix_web::{App, HttpResponse, HttpServer, Result, get, post, web};
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;
use uuid::Uuid;

type ProgressTracker = Arc<RwLock<HashMap<String, ProgressStatus>>>;

#[derive(Clone, Serialize, Deserialize)]
struct ProgressStatus {
    stage: String,
    current: usize,
    total: usize,
    message: String,
    complete: bool,
    results: Vec<OcrResult>,
}

#[derive(Clone, Serialize, Deserialize)]
struct OcrResult {
    filename: String,
    text: String,
    success: bool,
    error: Option<String>,
    pages_processed: Option<usize>,
    total_pages: Option<usize>,
    estimated_time_seconds: Option<f64>,
}

#[derive(Serialize)]
struct UploadResponse {
    session_id: String,
    results: Vec<OcrResult>,
}

#[derive(Serialize, Deserialize)]
struct ChunkInfo {
    filename: String,
    page_range: String,
    file_size: u64,
    download_path: String,
}

#[derive(Serialize)]
struct SplitResponse {
    success: bool,
    original_filename: String,
    total_pages: usize,
    chunks: Vec<ChunkInfo>,
    error: Option<String>,
}

#[get("/status/{session_id}")]
async fn get_status(
    path: web::Path<String>,
    tracker: web::Data<ProgressTracker>,
) -> Result<HttpResponse> {
    let session_id = path.into_inner();
    let status = tracker.read().get(&session_id).cloned();

    Ok(HttpResponse::Ok().json(status))
}

#[post("/upload")]
async fn upload(
    mut payload: Multipart,
    tracker: web::Data<ProgressTracker>,
) -> Result<HttpResponse> {
    let session_id = Uuid::new_v4().to_string();
    let temp_dir = std::env::temp_dir();

    // Collect files first
    let mut files_to_process = Vec::new();

    while let Some(item) = payload.next().await {
        let mut field = item?;

        let filename = field
            .content_disposition()
            .and_then(|cd| cd.get_filename())
            .unwrap_or("unnamed")
            .to_string();

        // Validate file extension
        let is_valid = filename.to_lowercase().ends_with(".pdf")
            || filename.to_lowercase().ends_with(".png")
            || filename.to_lowercase().ends_with(".jpg")
            || filename.to_lowercase().ends_with(".jpeg");

        if !is_valid {
            continue;
        }

        // Generate unique filename and save
        let file_id = Uuid::new_v4();
        let extension = filename.split('.').last().unwrap_or("tmp");
        let temp_path = temp_dir.join(format!("ocr_{}.{}", file_id, extension));

        let mut file = std::fs::File::create(&temp_path)?;
        while let Some(chunk) = field.next().await {
            let data = chunk?;
            file.write_all(&data)?;
        }
        file.flush()?;

        files_to_process.push((temp_path, filename));
    }

    // Spawn background task to process files
    let session_id_clone = session_id.clone();
    let tracker_clone = tracker.get_ref().clone();

    tokio::spawn(async move {
        let mut results = Vec::new();

        for (temp_path, filename) in files_to_process {
            let ocr_result =
                process_with_tesseract(&temp_path, &filename, &session_id_clone, &tracker_clone)
                    .await;
            results.push(ocr_result);
            let _ = std::fs::remove_file(&temp_path);
        }

        // Mark as complete with results
        tracker_clone.write().insert(
            session_id_clone.clone(),
            ProgressStatus {
                stage: "Complete".to_string(),
                current: results.len(),
                total: results.len(),
                message: "Processing complete".to_string(),
                complete: true,
                results: results.clone(),
            },
        );
    });

    // Return immediately with session_id
    Ok(HttpResponse::Ok().json(UploadResponse {
        session_id,
        results: vec![], // Results will be available via status endpoint
    }))
}

async fn process_with_tesseract(
    file_path: &std::path::Path,
    original_filename: &str,
    session_id: &str,
    tracker: &ProgressTracker,
) -> OcrResult {
    // Check if the file is a PDF
    let is_pdf = file_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase() == "pdf")
        .unwrap_or(false);

    // If it's a PDF, convert to images first (ALL pages)
    let image_paths = if is_pdf {
        // Initial status - we don't know page count yet
        tracker.write().insert(
            session_id.to_string(),
            ProgressStatus {
                stage: "Converting PDF".to_string(),
                current: 0,
                total: 0,
                message: format!("Converting PDF '{}'...", original_filename),
                complete: false,
                results: vec![],
            },
        );

        let temp_dir = std::env::temp_dir();
        let output_base = temp_dir.join(format!("pdf_convert_{}", Uuid::new_v4()));
        let output_prefix = output_base.to_str().unwrap();

        println!("Converting PDF '{}' to images...", original_filename);

        let convert_result = Command::new("pdftoppm")
            .arg("-png")
            .arg(file_path)
            .arg(output_prefix)
            .output();

        match convert_result {
            Ok(result) => {
                if result.status.success() {
                    // pdftoppm has inconsistent padding - check both formats
                    let mut pages = Vec::new();
                    let mut page_num = 1;
                    loop {
                        // Try 3-digit format first (001, 002, etc)
                        let mut png_path = format!("{}-{:03}.png", output_prefix, page_num);
                        if !std::path::Path::new(&png_path).exists() {
                            // Try 2-digit format (01, 02, etc)
                            png_path = format!("{}-{:02}.png", output_prefix, page_num);
                            if !std::path::Path::new(&png_path).exists() {
                                break;
                            }
                        }
                        pages.push(png_path);
                        page_num += 1;
                    }

                    if pages.is_empty() {
                        return OcrResult {
                            filename: original_filename.to_string(),
                            text: String::new(),
                            success: false,
                            error: Some(
                                "PDF conversion failed: no output files created".to_string(),
                            ),
                            pages_processed: None,
                            total_pages: None,
                            estimated_time_seconds: None,
                        };
                    }

                    println!("Converted {} pages from PDF", pages.len());

                    // Update progress with actual page count
                    tracker.write().insert(
                        session_id.to_string(),
                        ProgressStatus {
                            stage: "PDF Converted".to_string(),
                            current: pages.len(),
                            total: pages.len(),
                            message: format!("Converted {} pages, starting OCR...", pages.len()),
                            complete: false,
                            results: vec![],
                        },
                    );

                    Some(pages)
                } else {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    return OcrResult {
                        filename: original_filename.to_string(),
                        text: String::new(),
                        success: false,
                        error: Some(format!(
                            "PDF conversion error: {}. Make sure poppler-utils is installed.",
                            stderr
                        )),
                        pages_processed: None,
                        total_pages: None,
                        estimated_time_seconds: None,
                    };
                }
            }
            Err(e) => {
                return OcrResult {
                    filename: original_filename.to_string(),
                    text: String::new(),
                    success: false,
                    error: Some(format!(
                        "Failed to execute pdftoppm: {}. Install poppler-utils package.",
                        e
                    )),
                    pages_processed: None,
                    total_pages: None,
                    estimated_time_seconds: None,
                };
            }
        }
    } else {
        None
    };

    // Process pages or single image
    let mut all_text = String::new();

    if let Some(ref pages) = image_paths {
        // Process multiple pages from PDF with time estimation
        let total_pages = pages.len();
        println!(
            "Processing {} pages with Tesseract OCR (Sanskrit)...",
            total_pages
        );

        let mut estimated_time: Option<f64> = None;
        let start_time = std::time::Instant::now();

        for (idx, page_path) in pages.iter().enumerate() {
            let _page_start = std::time::Instant::now();

            // Update progress
            tracker.write().insert(
                session_id.to_string(),
                ProgressStatus {
                    stage: "OCR Processing".to_string(),
                    current: idx + 1,
                    total: total_pages,
                    message: format!("Processing page {}/{}", idx + 1, total_pages),
                    complete: false,
                    results: vec![],
                },
            );

            // After first page, calculate estimated remaining time
            if idx == 1 && estimated_time.is_none() {
                let first_page_time = start_time.elapsed().as_secs_f64();
                let remaining_pages = total_pages - 1;
                let estimated_total = first_page_time * total_pages as f64;
                estimated_time = Some(estimated_total);

                println!("  ⏱  First page took {:.1}s", first_page_time);
                println!(
                    "  📊 Estimated total time: {:.1}s ({:.1} minutes)",
                    estimated_total,
                    estimated_total / 60.0
                );
                println!(
                    "  📈 Estimated completion: ~{} remaining pages",
                    remaining_pages
                );
            }

            let progress_percent = (idx + 1) as f64 / total_pages as f64 * 100.0;
            println!(
                "  [{:.1}%] Processing page {}/{}...",
                progress_percent,
                idx + 1,
                total_pages
            );

            let temp_dir = std::env::temp_dir();
            let output_base = temp_dir.join(format!("ocr_output_{}", Uuid::new_v4()));
            let output_path = format!("{}", output_base.display());

            let output = Command::new("tesseract")
                .arg(page_path)
                .arg(&output_path)
                .arg("-l")
                .arg("san")
                .output();

            match output {
                Ok(result) => {
                    if result.status.success() {
                        let txt_file = format!("{}.txt", output_path);
                        if let Ok(text) = std::fs::read_to_string(&txt_file) {
                            if !text.trim().is_empty() {
                                all_text.push_str(&format!("\n━━━ Page {} ━━━\n", idx + 1));
                                all_text.push_str(&text);
                            }
                            let _ = std::fs::remove_file(&txt_file);
                        }
                    }
                }
                Err(_) => {
                    println!("  ⚠️  Warning: Failed to OCR page {}", idx + 1);
                }
            }

            if idx > 0 && idx % 10 == 0 {
                let elapsed = start_time.elapsed().as_secs_f64();
                let avg_time_per_page = elapsed / (idx + 1) as f64;
                let remaining = (total_pages - idx - 1) as f64 * avg_time_per_page;
                println!(
                    "  ⏰ Avg: {:.1}s/page | Remaining: ~{:.1}s ({:.1} min)",
                    avg_time_per_page,
                    remaining,
                    remaining / 60.0
                );
            }
        }

        // Clean up all converted images
        for page_path in pages {
            let _ = std::fs::remove_file(page_path);
        }

        let total_time = start_time.elapsed().as_secs_f64();
        println!(
            "✅ OCR completed for '{}': {} total characters in {:.1}s ({:.1} min)",
            original_filename,
            all_text.len(),
            total_time,
            total_time / 60.0
        );

        OcrResult {
            filename: original_filename.to_string(),
            text: all_text.trim().to_string(),
            success: true,
            error: None,
            pages_processed: Some(total_pages),
            total_pages: Some(total_pages),
            estimated_time_seconds: Some(total_time),
        }
    } else {
        // Process single image file
        let assets_dir = std::path::PathBuf::from("./assets/conversions");
        let output_base = assets_dir.join(format!("ocr_output_{}", Uuid::new_v4()));
        let output_path = format!("{}", output_base.display());

        let start_time = std::time::Instant::now();

        let output = Command::new("tesseract")
            .arg(file_path.to_str().unwrap())
            .arg(&output_path)
            .arg("-l")
            .arg("san")
            .output();

        match output {
            Ok(result) => {
                if result.status.success() {
                    let txt_file = format!("{}.txt", output_path);
                    match std::fs::read_to_string(&txt_file) {
                        Ok(text) => {
                            let _ = std::fs::remove_file(&txt_file);
                            let processing_time = start_time.elapsed().as_secs_f64();
                            println!(
                                "OCR Success for '{}': {} chars extracted in {:.1}s",
                                original_filename,
                                text.len(),
                                processing_time
                            );
                            if text.is_empty() {
                                println!("  WARNING: Empty text extracted!");
                            }

                            OcrResult {
                                filename: original_filename.to_string(),
                                text: text.trim().to_string(),
                                success: true,
                                error: None,
                                pages_processed: Some(1),
                                total_pages: Some(1),
                                estimated_time_seconds: Some(processing_time),
                            }
                        }
                        Err(e) => OcrResult {
                            filename: original_filename.to_string(),
                            text: String::new(),
                            success: false,
                            error: Some(format!("Failed to read OCR output: {}", e)),
                            pages_processed: None,
                            total_pages: None,
                            estimated_time_seconds: None,
                        },
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    OcrResult {
                        filename: original_filename.to_string(),
                        text: String::new(),
                        success: false,
                        error: Some(format!("Tesseract error: {}", stderr)),
                        pages_processed: None,
                        total_pages: None,
                        estimated_time_seconds: None,
                    }
                }
            }
            Err(e) => OcrResult {
                filename: original_filename.to_string(),
                text: String::new(),
                success: false,
                error: Some(format!(
                    "Failed to execute tesseract: {}. Make sure tesseract is installed.",
                    e
                )),
                pages_processed: None,
                total_pages: None,
                estimated_time_seconds: None,
            },
        }
    }
}

#[post("/split")]
async fn split_pdf(mut payload: Multipart) -> Result<HttpResponse> {
    let splits_dir = std::path::PathBuf::from("./assets/conversions/splits");
    std::fs::create_dir_all(&splits_dir)?;

    let mut field = match payload.next().await {
        Some(item) => item?,
        None => {
            return Ok(HttpResponse::BadRequest().json(SplitResponse {
                success: false,
                original_filename: String::new(),
                total_pages: 0,
                chunks: Vec::new(),
                error: Some("No file uploaded".to_string()),
            }));
        }
    };

    let filename = field
        .content_disposition()
        .and_then(|cd| cd.get_filename())
        .unwrap_or("unnamed.pdf")
        .to_string();

    // Validate PDF
    if !filename.to_lowercase().ends_with(".pdf") {
        return Ok(HttpResponse::BadRequest().json(SplitResponse {
            success: false,
            original_filename: filename,
            total_pages: 0,
            chunks: Vec::new(),
            error: Some("Only PDF files are supported for splitting".to_string()),
        }));
    }

    // Save uploaded PDF
    let file_id = Uuid::new_v4();
    let split_session_dir = splits_dir.join(file_id.to_string());
    std::fs::create_dir_all(&split_session_dir)?;

    let input_path = split_session_dir.join("original.pdf");
    let mut file = std::fs::File::create(&input_path)?;
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        file.write_all(&data)?;
    }
    file.flush()?;

    // Get PDF info using pdftk
    println!("Analyzing PDF '{}'...", filename);
    let dump_output = Command::new("pdftk")
        .arg(&input_path)
        .arg("dump_data")
        .output();

    let total_pages = match dump_output {
        Ok(result) => {
            if result.status.success() {
                let output_str = String::from_utf8_lossy(&result.stdout);
                // Parse "NumberOfPages: N"
                output_str
                    .lines()
                    .find(|line| line.starts_with("NumberOfPages:"))
                    .and_then(|line| line.split(':').nth(1))
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(0)
            } else {
                return Ok(HttpResponse::InternalServerError().json(SplitResponse {
                    success: false,
                    original_filename: filename,
                    total_pages: 0,
                    chunks: Vec::new(),
                    error: Some(
                        "Failed to analyze PDF with pdftk. Make sure pdftk is installed."
                            .to_string(),
                    ),
                }));
            }
        }
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(SplitResponse {
                success: false,
                original_filename: filename,
                total_pages: 0,
                chunks: Vec::new(),
                error: Some("pdftk not found. Please install: sudo apt install pdftk".to_string()),
            }));
        }
    };

    if total_pages == 0 {
        return Ok(HttpResponse::InternalServerError().json(SplitResponse {
            success: false,
            original_filename: filename,
            total_pages: 0,
            chunks: Vec::new(),
            error: Some("Could not determine PDF page count".to_string()),
        }));
    }

    // Calculate pages per chunk (~500KB target, estimate 10KB per page)
    let file_size_kb = std::fs::metadata(&input_path)?.len() / 1024;
    let estimated_kb_per_page = (file_size_kb as f64 / total_pages as f64).max(1.0);
    let pages_per_chunk = ((500.0 / estimated_kb_per_page).floor() as usize)
        .max(1)
        .min(total_pages);

    println!(
        "Splitting {} pages into chunks of ~{} pages each...",
        total_pages, pages_per_chunk
    );

    // Split PDF into chunks
    let mut chunks = Vec::new();
    let mut current_page = 1;
    let mut chunk_num = 1;

    while current_page <= total_pages {
        let end_page = (current_page + pages_per_chunk - 1).min(total_pages);
        let chunk_filename = format!(
            "chunk_{:03}_pages_{}-{}.pdf",
            chunk_num, current_page, end_page
        );
        let chunk_path = split_session_dir.join(&chunk_filename);

        println!(
            "  Creating chunk {}: pages {}-{}",
            chunk_num, current_page, end_page
        );

        let split_output = Command::new("pdftk")
            .arg(&input_path)
            .arg("cat")
            .arg(format!("{}-{}", current_page, end_page))
            .arg("output")
            .arg(&chunk_path)
            .output();

        match split_output {
            Ok(result) if result.status.success() => {
                if let Ok(metadata) = std::fs::metadata(&chunk_path) {
                    let download_path = format!("/downloads/{}/{}", file_id, chunk_filename);
                    chunks.push(ChunkInfo {
                        filename: chunk_filename,
                        page_range: format!("{}-{}", current_page, end_page),
                        file_size: metadata.len(),
                        download_path,
                    });
                }
            }
            _ => {
                println!("  Warning: Failed to create chunk {}", chunk_num);
            }
        }

        current_page = end_page + 1;
        chunk_num += 1;
    }

    println!("✅ Split complete: {} chunks created", chunks.len());

    Ok(HttpResponse::Ok().json(SplitResponse {
        success: true,
        original_filename: filename,
        total_pages,
        chunks,
        error: None,
    }))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("Starting Sanskrit OCR server at http://127.0.0.1:8080");

    // Create progress tracker
    let progress_tracker: ProgressTracker = Arc::new(RwLock::new(HashMap::new()));

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(progress_tracker.clone()))
            .service(get_status)
            .service(upload)
            .service(split_pdf)
            .service(
                fs::Files::new("/downloads", "./assets/conversions/splits").show_files_listing(),
            )
            .service(fs::Files::new("/", "./public").index_file("index.html"))
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}
