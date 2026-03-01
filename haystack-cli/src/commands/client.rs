use haystack_client::HaystackClient;
use haystack_core::codecs;

pub fn run_about(url: &str, username: &str, password: &str, format: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.about().await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

pub fn run_read(
    url: &str,
    username: &str,
    password: &str,
    filter: &str,
    limit: Option<usize>,
    format: &str,
) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.read(filter, limit).await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

pub fn run_nav(url: &str, username: &str, password: &str, nav_id: Option<&str>, format: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.nav(nav_id).await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

pub fn run_his_read(
    url: &str,
    username: &str,
    password: &str,
    id: &str,
    range: &str,
    format: &str,
) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.his_read(id, range).await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

pub fn run_ops(url: &str, username: &str, password: &str, format: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.ops().await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

pub fn run_libs(url: &str, username: &str, password: &str, format: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.libs().await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

pub fn run_specs(url: &str, username: &str, password: &str, lib: Option<&str>, format: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let client = connect(url, username, password).await;
        let grid = client.specs(lib).await.unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        });
        print_grid(&grid, format);
    });
}

async fn connect(
    url: &str,
    username: &str,
    password: &str,
) -> HaystackClient<haystack_client::transport::http::HttpTransport> {
    HaystackClient::connect(url, username, password)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Connection failed: {}", e);
            std::process::exit(1);
        })
}

fn print_grid(grid: &haystack_core::data::HGrid, format: &str) {
    let mime = match format {
        "json" => "application/json",
        "trio" => "text/trio",
        "json3" => "application/json;v=3",
        _ => "text/zinc",
    };
    let codec = codecs::codec_for(mime).unwrap();
    match codec.encode_grid(grid) {
        Ok(s) => println!("{}", s),
        Err(e) => {
            eprintln!("Encoding error: {}", e);
            std::process::exit(1);
        }
    }
}
