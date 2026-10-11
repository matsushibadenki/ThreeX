#[cfg(target_arch = "wasm32")]
mod browser {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    pub struct Runtime {
        scene: threex_core::Scene,
        renderer: threex_renderer::Renderer,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    }
    #[wasm_bindgen]
    impl Runtime {
        #[wasm_bindgen(js_name=create)]
        pub async fn create(canvas: web_sys::HtmlCanvasElement) -> Result<Runtime, JsValue> {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::BROWSER_WEBGPU,
                ..Default::default()
            });
            let surface = instance
                .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: Some(&surface),
                    ..Default::default()
                })
                .await
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let config = surface
                .get_default_config(&adapter, 1, 1)
                .ok_or_else(|| JsValue::from_str("No surface format"))?;
            surface.configure(&device, &config);
            let renderer = threex_renderer::Renderer::new(device, queue, config.format);
            Ok(Self {
                scene: Default::default(),
                renderer,
                surface,
                config,
            })
        }
        pub fn render(
            &mut self,
            commands: &[u8],
            vp: &[f32],
            width: u32,
            height: u32,
        ) -> Result<(), JsValue> {
            if vp.len() != 16 || vp.iter().any(|v| !v.is_finite()) {
                return Err(JsValue::from_str("Invalid camera"));
            }
            self.scene
                .apply(commands)
                .map_err(|e| JsValue::from_str(&e))?;
            if width == 0 || height == 0 {
                return Ok(());
            }
            if self.config.width != width || self.config.height != height {
                self.config.width = width;
                self.config.height = height;
                self.surface.configure(&self.renderer.device, &self.config);
            }
            let frame = match self.surface.get_current_texture() {
                Ok(frame) => frame,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    self.surface.configure(&self.renderer.device, &self.config);
                    self.surface
                        .get_current_texture()
                        .map_err(|e| JsValue::from_str(&e.to_string()))?
                }
                Err(error) => return Err(JsValue::from_str(&error.to_string())),
            };
            self.renderer.render(
                &self.scene,
                &frame.texture.create_view(&Default::default()),
                width,
                height,
                vp.try_into().unwrap(),
            );
            frame.present();
            Ok(())
        }
    }
}
