use std::collections::HashMap;

use hyper::{header, Body, Request, Response, StatusCode};

use regex::Regex;

use serde_json::json;

use crate::tiles::{get_grid_data, get_tile_data, TileMeta, TileSummaryJSON};
use crate::utils::{encode, get_blank_image, DataFormat};

lazy_static! {
    static ref TILE_URL_RE: Regex =
        Regex::new(r"^/services/(?P<tile_path>.*)/tiles/(?P<z>\d+)/(?P<x>\d+)/(?P<y>\d+)\.(?P<format>[a-zA-Z]+)/?(\?(?P<query>.*))?").unwrap();
}

#[allow(dead_code)]
static INTERNAL_SERVER_ERROR: &[u8] = b"Internal Server Error";
static NOT_FOUND: &[u8] = b"Not Found";
static NO_CONTENT: &[u8] = b"";

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(NOT_FOUND.into())
        .unwrap()
}

fn no_content() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(NO_CONTENT.into())
        .unwrap()
}

#[allow(dead_code)]
fn server_error() -> Response<Body> {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(INTERNAL_SERVER_ERROR.into())
        .unwrap()
}

fn bad_request(msg: String) -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(msg))
        .unwrap()
}

pub fn tile_map() -> Response<Body> {
    let css = include_str!("../templates/static/dist/core.min.css");
    let js = include_str!("../templates/static/dist/core.min.js");
    let html = include_str!("../templates/map.html");
    let body = Body::from(html.replace("{{css}}", css).replace("{{js}}", js));
    Response::new(body)
}

pub async fn get_service(
    request: Request<Body>,
    tilesets: HashMap<String, TileMeta>,
) -> Result<Response<Body>, hyper::Error> {
    let path = request.uri().path();
    let scheme = match request.uri().scheme_str() {
        Some(scheme) => format!("{}://", scheme),
        None => String::from("http://"),
    };
    let base_url = format!(
        "{}{}/services",
        scheme,
        request.headers()["host"].to_str().unwrap()
    );

    match TILE_URL_RE.captures(path) {
        Some(matches) => {
            let tile_path = matches.name("tile_path").unwrap().as_str();
            let tile_meta = tilesets.get(tile_path).unwrap();
            let z = matches.name("z").unwrap().as_str().parse::<u32>().unwrap();
            let x = matches.name("x").unwrap().as_str().parse::<u32>().unwrap();
            let y = matches.name("y").unwrap().as_str().parse::<u32>().unwrap();
            let y: u32 = (1 << z) - 1 - y;
            let data_format = matches.name("format").unwrap().as_str();
            // For future use
            let _query_string = match matches.name("query") {
                Some(q) => q.as_str(),
                None => "",
            };

            return match data_format {
                "json" => match tile_meta.grid_format {
                    Some(grid_format) => match get_grid_data(
                        &tile_meta.connection_pool.get().unwrap(),
                        grid_format,
                        z,
                        x,
                        y,
                    ) {
                        Ok(data) => {
                            let data = serde_json::to_vec(&data).unwrap();
                            Ok(Response::builder()
                                .header(header::CONTENT_TYPE, DataFormat::JSON.content_type())
                                .header(header::CONTENT_ENCODING, "gzip")
                                .body(Body::from(encode(&data)))
                                .unwrap())
                        }
                        Err(_) => Ok(no_content()),
                    },
                    None => Ok(not_found()),
                },
                "pbf" => match get_tile_data(&tile_meta.connection_pool.get().unwrap(), z, x, y) {
                    Ok(data) => Ok(Response::builder()
                        .header(header::CONTENT_TYPE, DataFormat::PBF.content_type())
                        .header(header::CONTENT_ENCODING, "gzip")
                        .body(Body::from(data))
                        .unwrap()),
                    Err(_) => Ok(no_content()),
                },
                _ => {
                    let data =
                        match get_tile_data(&tile_meta.connection_pool.get().unwrap(), z, x, y) {
                            Ok(data) => data,
                            Err(_) => get_blank_image(),
                        };
                    Ok(Response::builder()
                        .header(
                            header::CONTENT_TYPE,
                            DataFormat::new(data_format).content_type(),
                        )
                        .body(Body::from(data))
                        .unwrap())
                }
            };
        }
        None => {
            if path.starts_with("/services") {
                let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
                if segments.len() == 1 {
                    // Root url (/services): show all services
                    let mut tiles_summary = Vec::new();
                    for (tile_name, tile_meta) in tilesets {
                        tiles_summary.push(TileSummaryJSON {
                            image_type: tile_meta.tile_format,
                            url: format!("{}/{}", base_url, tile_name),
                        });
                    }
                    let resp_json = serde_json::to_string(&tiles_summary).unwrap(); // TODO handle error
                    return Ok(Response::builder()
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(resp_json))
                        .unwrap()); // TODO handle error
                }

                if segments[segments.len() - 1] == "map" {
                    // Tileset map preview (/services/<tileset-path>/map)
                    return Ok(tile_map());
                }

                // Tileset details (/services/<tileset-path>)
                let tile_name = segments[1..].join("/");
                let tile_meta = match tilesets.get(&tile_name) {
                    Some(tile_meta) => tile_meta.clone(),
                    None => {
                        return Ok(bad_request(format!(
                            "Tileset does not exist: {}",
                            tile_name
                        )))
                    }
                };
                let query_string = match request.uri().query() {
                    Some(q) => format!("?{}", q),
                    None => String::new(),
                };

                let mut tile_meta_json = json!({
                    "name": tile_meta.name,
                    "version": tile_meta.version,
                    "map": format!("{}/{}/{}", base_url, tile_name, "map"),
                    "tiles": vec![format!(
                        "{}/{}/tiles/{{z}}/{{x}}/{{y}}.{}{}",
                        base_url,
                        tile_name,
                        tile_meta.tile_format.format(),
                        query_string
                    )],
                    "tilejson": tile_meta.tilejson,
                    "scheme": tile_meta.scheme,
                    "id": tile_meta.id,
                    "format": tile_meta.tile_format,
                    "grids": match tile_meta.grid_format {
                        Some(_) => Some(vec![format!(
                            "{}/{}/tiles/{{z}}/{{x}}/{{y}}.json{}",
                            base_url, tile_name, query_string
                        )]),
                        None => None,
                    },
                    "bounds": tile_meta.bounds,
                    "center": tile_meta.center,
                    "minzoom": tile_meta.minzoom,
                    "maxzoom": tile_meta.maxzoom,
                    "description": tile_meta.description,
                    "attribution": tile_meta.attribution,
                    "type": tile_meta.layer_type,
                    "legend": tile_meta.legend,
                    "template": tile_meta.template,
                });
                match tile_meta.json {
                    Some(json_data) => {
                        for (k, v) in json_data.as_object().unwrap() {
                            tile_meta_json[k] = v.clone();
                        }
                    }
                    None => (),
                };

                return Ok(Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&tile_meta_json).unwrap()))
                    .unwrap()); // TODO handle error
            }
        }
    };
    Ok(not_found())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::discover_tilesets;
    use crate::utils::decode;
    use hyper::body;
    use serde_json::Value as JSONValue;
    use std::path::PathBuf;

    async fn setup(uri: &str) -> Response<Body> {
        let request = Request::get(uri)
            .header("host", "localhost:3000")
            .body(Body::from(""))
            .unwrap();

        let tilesets = discover_tilesets(String::new(), PathBuf::from("./tiles"));
        get_service(request, tilesets).await.unwrap()
    }

    #[tokio::test]
    async fn get_services() {
        let response = setup("http://localhost:3000/services").await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn get_details() {
        let response = setup("http://localhost:3000/services/geography-class-png").await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn get_preview_map() {
        let response = setup("http://localhost:3000/services/geography-class-png/map").await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn get_existing_tile() {
        let response =
            setup("http://localhost:3000/services/geography-class-png/tiles/0/0/0.png").await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn get_non_existing_tile() {
        // Geography Class PNG has no tiles beyond zoom level 1 and should return a blank image
        let response =
            setup("http://localhost:3000/services/geography-class-png/tiles/2/0/0.png").await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            body::to_bytes(response.into_body()).await.unwrap(),
            get_blank_image()
        );
    }

    #[tokio::test]
    async fn get_existing_utfgrid_data() {
        let response =
            setup("http://localhost:3000/services/geography-class-png/tiles/0/0/0.json").await;
        assert_eq!(response.status(), 200);
        let data: JSONValue = serde_json::from_str(
            &decode(
                body::to_bytes(response.into_body()).await.unwrap().to_vec(),
                DataFormat::GZIP,
            )
            .unwrap(),
        )
        .unwrap();
        assert_ne!(data.get("data"), None);
        assert_ne!(data.get("grid"), None);
        assert_ne!(data.get("keys"), None);
    }

    #[tokio::test]
    async fn get_non_existing_utfgrid_data() {
        // should return empty content with 204 status
        let response =
            setup("http://localhost:3000/services/geography-class-png/tiles/2/0/0.json").await;
        assert_eq!(response.status(), 204);
    }
}
