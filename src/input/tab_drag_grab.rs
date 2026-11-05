use smithay::backend::input::{ButtonState, InputTime};
use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, GestureHoldBeginEvent,
    GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
    GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::output::Output;
use smithay::utils::{IsAlive, Logical, Point, SERIAL_COUNTER};

use crate::input::State;

/// Grab that recognizes a drag on a tab indicator and drags the tab.
///
/// Until the pointer moves far enough, the grab just recognizes the gesture; clicking a tab
/// without dragging releases the grab with no effect. Once recognized, the layout's tab drag
/// state is set and updated on motion; the drop action is applied when the start button is
/// released.
pub struct TabDragGrab {
    start_data: PointerGrabStartData<State>,
    output: Output,
    start_pos_within_output: Point<f64, Logical>,
    window: Window,
    gesture: GestureState,
    new_location: Point<f64, Logical>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GestureState {
    Recognizing,
    Dragging,
}

impl TabDragGrab {
    pub fn new(
        start_data: PointerGrabStartData<State>,
        output: &Output,
        start_pos_within_output: Point<f64, Logical>,
        window: Window,
    ) -> Self {
        Self {
            new_location: start_data.location,
            start_data,
            output: output.clone(),
            start_pos_within_output,
            window,
            gesture: GestureState::Recognizing,
        }
    }

    fn begin_drag(&mut self, data: &mut State) -> bool {
        if !data.niri.layout.tab_drag_begin(
            self.window.clone(),
            &self.output,
            self.start_pos_within_output,
        ) {
            return false;
        }

        self.gesture = GestureState::Dragging;
        data.niri
            .cursor_manager
            .set_cursor_image(CursorImageStatus::Named(CursorIcon::Grabbing));
        true
    }

    fn on_frame(&mut self, data: &mut State) -> bool {
        match self.gesture {
            GestureState::Recognizing => {
                if !self.window.alive() {
                    return false;
                }

                let delta = self.new_location - self.start_data.location;
                if delta.x * delta.x + delta.y * delta.y >= 64. {
                    if !self.begin_drag(data) {
                        return false;
                    }
                } else {
                    return true;
                }
            }
            GestureState::Dragging => {
                if !self.window.alive() {
                    data.niri.layout.tab_drag_cancel();
                    return false;
                }
            }
        }

        let found = data.niri.output_under(self.new_location);
        let found = found.map(|(output, pos_within_output)| (output.clone(), pos_within_output));
        if let Some((output, pos_within_output)) = found {
            data.niri
                .layout
                .tab_drag_update(Some((&output, pos_within_output)));
        } else {
            data.niri.layout.tab_drag_update(None);
        }

        // FIXME: granular.
        data.niri.queue_redraw_all();
        true
    }

    fn on_ungrab(&mut self, data: &mut State) {
        if self.gesture == GestureState::Dragging {
            data.niri.layout.tab_drag_end();
        }

        data.niri
            .cursor_manager
            .set_cursor_image(CursorImageStatus::default_named());

        // FIXME: granular.
        data.niri.queue_redraw_all();
    }
}

impl PointerGrab<State> for TabDragGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(
            <State as smithay::input::SeatHandler>::PointerFocus,
            Point<f64, Logical>,
        )>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus.
        handle.motion(data, None, event);
        self.new_location = event.location;
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(
            <State as smithay::input::SeatHandler>::PointerFocus,
            Point<f64, Logical>,
        )>,
        event: &RelativeMotionEvent,
    ) {
        // While the grab is active, no client has pointer focus.
        handle.relative_motion(data, None, event);
        self.new_location += event.delta;
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        if event.state == ButtonState::Released && event.button == self.start_data.button {
            self.on_ungrab(data);
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);

        if !self.on_frame(data) {
            // The drag is no longer ongoing.
            self.on_ungrab(data);
            handle.unset_grab(
                self,
                data,
                SERIAL_COUNTER.next_serial(),
                InputTime::now(),
                true,
            );
        }
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut State) {
        self.on_ungrab(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }
}
