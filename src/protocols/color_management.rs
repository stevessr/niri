use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{self as output, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{
        self as feedback, WpColorManagementSurfaceFeedbackV1,
    },
    wp_color_management_surface_v1::{self as surface, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{self as manager, WpColorManagerV1},
    wp_image_description_info_v1::WpImageDescriptionInfoV1,
    wp_image_description_v1::{self as description, WpImageDescriptionV1},
};
use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::wayland::{compositor, Dispatch2, GlobalDispatch2};

use crate::color_management::{
    SurfaceColorDescription, SurfaceColorState, SurfaceRenderIntent,
};

const VERSION: u32 = 1;
const SRGB_IDENTITY: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColorDescription {
    Srgb,
    /// The output is currently using niri's experimental HDR path. Until the compositor has a
    /// linear-light per-surface working space, do not publish a ready HDR image description that a
    /// client could legally attach back to a surface.
    UnsupportedHdr,
    Gone,
}

pub trait ColorManagementHandler {
    fn color_management_state(&mut self) -> &mut ColorManagementManagerState;
    fn output_color_description(&self, output: &WlOutput) -> OutputColorDescription;
}

pub struct ColorManagementManagerState {
    global: GlobalId,
    pending_info_done: Vec<WpImageDescriptionInfoV1>,
}

impl ColorManagementManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ColorManagementGlobalData>
            + Dispatch<WpColorManagerV1, ColorManagerData>
            + Dispatch<WpColorManagementOutputV1, OutputData>
            + Dispatch<WpColorManagementSurfaceV1, SurfaceData>
            + Dispatch<WpColorManagementSurfaceFeedbackV1, SurfaceFeedbackData>
            + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
            + Dispatch<WpImageDescriptionInfoV1, ()>
            + ColorManagementHandler
            + 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ColorManagementGlobalData {
            filter: Box::new(filter),
        };
        let global = display.create_global::<D, WpColorManagerV1, _>(VERSION, global_data);

        Self {
            global,
            pending_info_done: Vec::new(),
        }
    }

    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    /// Finish information objects after the dispatch callback that created them has returned.
    ///
    /// wp_image_description_info_v1.done is a destructor event. Deferring it avoids destroying a
    /// newly initialized resource before wayland-server has finished installing its object data.
    pub fn flush_pending(&mut self) {
        for info in self.pending_info_done.drain(..) {
            info.done();
        }
    }
}

pub struct ColorManagementGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

#[derive(Debug)]
pub struct ColorManagerData;

#[derive(Debug)]
pub struct OutputData {
    output: Weak<WlOutput>,
}

#[derive(Debug)]
pub struct SurfaceFeedbackData {
    surface: Weak<WlSurface>,
}

#[derive(Debug)]
pub struct SurfaceData {
    surface: Weak<WlSurface>,
}

#[derive(Debug)]
struct SurfaceAttachmentState {
    attached: AtomicBool,
}

impl SurfaceAttachmentState {
    fn new() -> Self {
        Self {
            attached: AtomicBool::new(false),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImageDescriptionData {
    color: SurfaceColorDescription,
    information: bool,
    ready: bool,
}

fn init_srgb_description<D>(
    data_init: &mut DataInit<'_, D>,
    id: New<WpImageDescriptionV1>,
    information: bool,
) -> WpImageDescriptionV1
where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let image = data_init.init(
        id,
        ImageDescriptionData {
            color: SurfaceColorDescription::Srgb,
            information,
            ready: true,
        },
    );
    image.ready(SRGB_IDENTITY);
    image
}

fn init_failed_description<D>(
    data_init: &mut DataInit<'_, D>,
    id: New<WpImageDescriptionV1>,
    cause: description::Cause,
    message: &str,
) where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    data_init
        .init(
            id,
            ImageDescriptionData {
                color: SurfaceColorDescription::Srgb,
                information: false,
                ready: false,
            },
        )
        .failed(cause, message.to_owned());
}

impl<D> GlobalDispatch2<WpColorManagerV1, D> for ColorManagementGlobalData
where
    D: Dispatch<WpColorManagerV1, ColorManagerData> + ColorManagementHandler + 'static,
{
    fn bind(
        &self,
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ColorManagerData);
        manager.supported_intent(manager::RenderIntent::Perceptual);
        manager.done();
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<WpColorManagerV1, D> for ColorManagerData
where
    D: Dispatch<WpColorManagementOutputV1, OutputData>
        + Dispatch<WpColorManagementSurfaceV1, SurfaceData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, SurfaceFeedbackData>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        manager: &WpColorManagerV1,
        request: manager::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            manager::Request::Destroy => {}
            manager::Request::GetOutput { id, output } => {
                data_init.init(
                    id,
                    OutputData {
                        output: output.downgrade(),
                    },
                );
            }
            manager::Request::GetSurface { id, surface } => {
                let already_attached = compositor::with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(SurfaceAttachmentState::new);
                    let attachment = states
                        .data_map
                        .get::<SurfaceAttachmentState>()
                        .expect("surface attachment state was just inserted");
                    attachment.attached.swap(true, Ordering::AcqRel)
                });

                if already_attached {
                    manager.post_error(
                        manager::Error::SurfaceExists,
                        "wl_surface already has a color-management object",
                    );
                    return;
                }

                data_init.init(
                    id,
                    SurfaceData {
                        surface: surface.downgrade(),
                    },
                );
            }
            manager::Request::GetSurfaceFeedback { id, surface } => {
                let feedback = data_init.init(
                    id,
                    SurfaceFeedbackData {
                        surface: surface.downgrade(),
                    },
                );
                feedback.preferred_changed(SRGB_IDENTITY);
            }
            _ => manager.post_error(
                manager::Error::UnsupportedFeature,
                "niri currently exposes only compositor-provided sRGB descriptions",
            ),
        }
    }
}

impl<D> Dispatch2<WpColorManagementOutputV1, D> for OutputData
where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementOutputV1,
        request: output::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            output::Request::Destroy => {}
            output::Request::GetImageDescription { image_description } => {
                let Some(output) = self.output.upgrade().ok() else {
                    init_failed_description(
                        data_init,
                        image_description,
                        description::Cause::NoOutput,
                        "wl_output is no longer available",
                    );
                    return;
                };

                match state.output_color_description(&output) {
                    OutputColorDescription::Srgb => {
                        init_srgb_description(data_init, image_description, true);
                    }
                    OutputColorDescription::UnsupportedHdr => {
                        init_failed_description(
                            data_init,
                            image_description,
                            description::Cause::Unsupported,
                            "native HDR output descriptions are withheld until per-surface HDR composition is implemented",
                        );
                    }
                    OutputColorDescription::Gone => {
                        init_failed_description(
                            data_init,
                            image_description,
                            description::Cause::NoOutput,
                            "output is no longer available",
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

impl<D> Dispatch2<WpColorManagementSurfaceFeedbackV1, D> for SurfaceFeedbackData
where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        resource: &WpColorManagementSurfaceFeedbackV1,
        request: feedback::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            feedback::Request::Destroy => {}
            feedback::Request::GetPreferred { image_description } => {
                if self.surface.upgrade().is_err() {
                    resource.post_error(feedback::Error::Inert, "wl_surface was destroyed");
                    return;
                }
                init_srgb_description(data_init, image_description, true);
            }
            _ => resource.post_error(
                feedback::Error::UnsupportedFeature,
                "parametric preferred descriptions are not advertised",
            ),
        }
    }
}

impl<D> Dispatch2<WpColorManagementSurfaceV1, D> for SurfaceData
where
    D: ColorManagementHandler + 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        resource: &WpColorManagementSurfaceV1,
        request: surface::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let wl_surface = self.surface.upgrade().ok();

        match request {
            surface::Request::Destroy => {
                if let Some(wl_surface) = wl_surface {
                    compositor::with_states(&wl_surface, |states| {
                        if let Some(attachment) =
                            states.data_map.get::<SurfaceAttachmentState>()
                        {
                            attachment.attached.store(false, Ordering::Release);
                        }

                        let mut color = states.cached_state.get::<SurfaceColorState>();
                        *color.pending() = SurfaceColorState::default();
                    });
                }
            }
            surface::Request::UnsetImageDescription => {
                let Some(wl_surface) = wl_surface else {
                    resource.post_error(surface::Error::Inert, "wl_surface was destroyed");
                    return;
                };
                compositor::with_states(&wl_surface, |states| {
                    let mut color = states.cached_state.get::<SurfaceColorState>();
                    *color.pending() = SurfaceColorState::default();
                });
            }
            surface::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let Some(wl_surface) = wl_surface else {
                    resource.post_error(surface::Error::Inert, "wl_surface was destroyed");
                    return;
                };

                if render_intent.into_result().ok() != Some(manager::RenderIntent::Perceptual) {
                    resource.post_error(
                        surface::Error::RenderIntent,
                        "only the perceptual render intent is supported",
                    );
                    return;
                }

                let Some(description) = image_description.data::<ImageDescriptionData>() else {
                    resource.post_error(
                        surface::Error::ImageDescription,
                        "unknown image description",
                    );
                    return;
                };
                if !description.ready {
                    resource.post_error(
                        surface::Error::ImageDescription,
                        "image description is not ready",
                    );
                    return;
                }

                compositor::with_states(&wl_surface, |states| {
                    let mut color = states.cached_state.get::<SurfaceColorState>();
                    *color.pending() = SurfaceColorState {
                        description: description.color,
                        render_intent: SurfaceRenderIntent::Perceptual,
                        explicitly_managed: true,
                    };
                });
            }
            _ => {}
        }
    }

    fn destroyed(&self, _state: &mut D, _client: ClientId, _resource: &WpColorManagementSurfaceV1) {
        // Graceful Destroy resets the double-buffered state. If the client disconnects, the
        // wl_surface lifetime owns the remaining cached state and no new commit can apply it.
    }
}

impl<D> Dispatch2<WpImageDescriptionV1, D> for ImageDescriptionData
where
    D: Dispatch<WpImageDescriptionInfoV1, ()> + ColorManagementHandler + 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionV1,
        request: description::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            description::Request::Destroy => {}
            description::Request::GetInformation { information } => {
                if !self.ready {
                    resource.post_error(
                        description::Error::NotReady,
                        "image description is not ready",
                    );
                    return;
                }
                if !self.information {
                    resource.post_error(
                        description::Error::NoInformation,
                        "image description does not expose information",
                    );
                    return;
                }

                let info = data_init.init(information, ());
                match self.color {
                    SurfaceColorDescription::Srgb => {
                        info.primaries(
                            640000, 330000, 300000, 600000, 150000, 60000, 312700, 329000,
                        );
                        info.primaries_named(manager::Primaries::Srgb);
                        info.tf_named(manager::TransferFunction::Srgb);
                        info.luminances(2000, 80, 80);
                    }
                }

                state.color_management_state().pending_info_done.push(info);
            }
            _ => {}
        }
    }
}

impl<D> Dispatch2<WpImageDescriptionInfoV1, D> for ()
where
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionInfoV1,
        _request: <WpImageDescriptionInfoV1 as Resource>::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

pub fn surface_color_state(surface: &WlSurface) -> SurfaceColorState {
    compositor::with_states(surface, |states| {
        let mut color = states.cached_state.get::<SurfaceColorState>();
        *color.current()
    })
}

pub fn output_exists(output: &WlOutput) -> bool {
    Output::from_resource(output).is_some()
}
