"""
Digital Picture Frame - GTK4 UI Module
Copyright (C) 2025 Daniel Mikusa

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as published
by the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
"""

import logging
import sys
import os
import gi

# Set basic environment to avoid accessibility warnings
os.environ.setdefault("GTK_A11Y", "none")

gi.require_version("Gtk", "4.0")
gi.require_version("Gio", "2.0")
gi.require_version("GLib", "2.0")
gi.require_version("Gdk", "4.0")

from gi.repository import Gtk, Gio, GLib, Gdk  # type: ignore[import-untyped]
from pathlib import Path
from typing import Optional, Any
from .config import FrameConfig
from .photos import PhotoLoader

logger = logging.getLogger(__name__)


class PictureFrameApp(Gtk.Application):
    """GTK4 application for the digital picture frame"""

    def __init__(self, config: FrameConfig, photo_loader: PhotoLoader):
        super().__init__(application_id="com.mikusa.picture-frame-ui")
        self.config = config
        self.photo_loader = photo_loader
        self.window: Optional[Any] = None  # Gtk.ApplicationWindow
        self.stack: Optional[Any] = None  # Gtk.Stack
        self.picture1: Optional[Any] = None  # Gtk.Picture
        self.picture2: Optional[Any] = None  # Gtk.Picture
        self.current_picture = 1  # Track which picture is currently visible
        self.slideshow_source_id: Optional[int] = None

        # Set up application-level actions
        self.setup_actions()

    def setup_actions(self):
        """Set up application-level actions and keyboard shortcuts"""
        # Quit action
        quit_action = Gio.SimpleAction.new("quit", None)
        quit_action.connect("activate", self._on_quit_action)
        self.add_action(quit_action)

        # Set up OS-specific keyboard accelerator
        import platform

        if platform.system() == "Darwin":  # macOS
            accelerator = "<Meta>q"  # Cmd+Q on macOS
            description = "Cmd+Q"
        else:
            accelerator = "<Control>q"  # Ctrl+Q on Windows/Linux
            description = "Ctrl+Q"

        self.set_accels_for_action("app.quit", [accelerator])
        logger.debug(
            f"Application quit action set up with keyboard shortcut: {description} ({accelerator})"
        )

    def _on_quit_action(self, action, parameter):
        """Handle quit action"""
        logger.debug("Quit action triggered from keyboard shortcut")
        self.quit()

    def do_activate(self):
        """Called when the application is activated"""
        logger.debug("App activation callback called")
        if self.window is None:
            self.build_ui()
        assert self.window is not None  # Type checker hint
        self.window.present()

    def build_ui(self):
        """Build the main UI components"""
        logger.debug("Building UI with crossfade animation")

        # Create the main application window
        self.window = Gtk.ApplicationWindow(application=self)
        assert self.window is not None  # Type checker hint
        self.window.set_title("Digital Picture Frame")
        self.window.set_default_size(800, 600)

        # Set black background for the window
        self._apply_black_background(self.window)

        # Set full screen mode if configured
        if self.config.full_screen:
            logger.debug("Setting window to full screen mode")
            self.window.fullscreen()

            # Hide the mouse cursor in full screen mode
            self._hide_mouse_cursor()

        # Create a vertical box to hold our UI elements
        vbox = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0)

        # Remove margins for full-screen picture frame display
        vbox.set_margin_top(0)
        vbox.set_margin_bottom(0)
        vbox.set_margin_start(0)
        vbox.set_margin_end(0)

        # Ensure the container fills the entire window
        vbox.set_hexpand(True)
        vbox.set_vexpand(True)

        # Set black background for the container
        self._apply_black_background(vbox)

        # Create a Stack widget for crossfade animation between images
        self.stack = Gtk.Stack()
        assert self.stack is not None  # Type checker hint
        self.stack.set_transition_type(Gtk.StackTransitionType.CROSSFADE)
        self.stack.set_transition_duration(self.config.fade_duration)
        self.stack.set_hhomogeneous(True)
        self.stack.set_vhomogeneous(True)

        # Set black background for the stack
        self._apply_black_background(self.stack)

        # Center the stack contents
        self.stack.set_halign(Gtk.Align.FILL)
        self.stack.set_valign(Gtk.Align.FILL)
        self.stack.set_hexpand(True)
        self.stack.set_vexpand(True)

        # Create two Picture widgets for double buffering the animation
        self.picture1 = self._create_picture_widget()
        self.picture2 = self._create_picture_widget()

        # Add both pictures to the stack with names
        self.stack.add_named(self.picture1, "picture1")
        self.stack.add_named(self.picture2, "picture2")

        # Start with picture1 visible
        self.stack.set_visible_child_name("picture1")

        # Load the first image into picture1
        self._load_image_into_picture(self.picture1)

        # Add the stack to the box (it will expand to fill available space)
        vbox.append(self.stack)

        # Set the box as the window's child
        self.window.set_child(vbox)

        # Start the slideshow timer
        self._start_slideshow_timer()

        logger.info("UI built with crossfade animation and slideshow timer started")

    def _create_picture_widget(self) -> Any:  # Returns Gtk.Picture
        """Create a configured Picture widget"""
        picture = Gtk.Picture()
        picture.set_can_shrink(True)
        picture.set_keep_aspect_ratio(True)
        picture.set_alternative_text("Digital Picture Frame Display")

        # Center the image both horizontally and vertically
        picture.set_halign(Gtk.Align.CENTER)
        picture.set_valign(Gtk.Align.CENTER)

        # Allow the widget to expand to fill available space
        picture.set_hexpand(True)
        picture.set_vexpand(True)

        return picture

    def _apply_black_background(self, widget: Any) -> None:
        """Apply black background to a widget using CSS"""
        css_provider = Gtk.CssProvider()
        css_data = """
        * {
            background-color: black;
            background: black;
        }
        """
        css_provider.load_from_string(css_data)

        # Apply the CSS to the widget
        style_context = widget.get_style_context()
        style_context.add_provider(
            css_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )

        logger.debug(f"Applied black background to widget: {type(widget).__name__}")

    def _hide_mouse_cursor(self) -> None:
        """Hide the mouse cursor when in full screen mode"""
        try:
            # Ensure window is initialized
            assert self.window is not None  # Type checker hint

            # Get the window's surface
            surface = self.window.get_surface()
            if surface is not None:
                # Create an empty cursor to hide the mouse
                display = self.window.get_display()
                cursor = Gdk.Cursor.new_from_name("none", None)
                if cursor is None:
                    # Fallback: create a blank cursor
                    cursor = Gdk.Cursor.new_from_name("blank", None)

                if cursor is not None:
                    surface.set_cursor(cursor)
                    logger.debug("Mouse cursor hidden in full screen mode")
                else:
                    logger.warning("Failed to create blank cursor")
            else:
                logger.warning("Could not get window surface to hide cursor")
        except Exception as e:
            logger.warning(f"Failed to hide mouse cursor: {e}")

    def _load_image_into_picture(self, picture: Any):  # picture: Gtk.Picture
        """Load the next image into the specified Picture widget"""
        try:
            photo_url = self.photo_loader.load_next_photo()
            logger.debug(f"Loading image: {photo_url}")

            # Convert file URL to GFile
            gfile = Gio.File.new_for_uri(photo_url)
            picture.set_file(gfile)

            # Get just the filename for logging
            path = Path(gfile.get_path())
            logger.info(f"Image loaded: {path.name}")

        except Exception as e:
            logger.warning(f"Failed to load next photo: {e}")
            picture.set_alternative_text("Failed to load image")

    def _start_slideshow_timer(self):
        """Start the automatic slideshow timer"""

        def on_timer():
            logger.debug("Timer triggered - loading next photo with crossfade")

            # Ensure stack is initialized
            assert self.stack is not None  # Type checker hint

            # Determine which picture to load next
            if self.current_picture == 1:
                next_picture = self.picture2
                next_name = "picture2"
                self.current_picture = 2
            else:
                next_picture = self.picture1
                next_name = "picture1"
                self.current_picture = 1

            # Load the next image into the hidden picture
            self._load_image_into_picture(next_picture)

            # Trigger crossfade to the newly loaded picture
            self.stack.set_visible_child_name(next_name)

            return True  # Continue the timer

        # Set up timer to trigger every slideshow_duration seconds
        self.slideshow_source_id = GLib.timeout_add_seconds(
            self.config.slideshow_duration, on_timer
        )
        logger.debug(
            f"Slideshow timer started with {self.config.slideshow_duration}s interval"
        )

    def _stop_slideshow_timer(self):
        """Stop the slideshow timer"""
        if self.slideshow_source_id is not None:
            GLib.source_remove(self.slideshow_source_id)
            self.slideshow_source_id = None
            logger.debug("Slideshow timer stopped")

    def do_shutdown(self):
        """Called when the application is shutting down"""
        logger.debug("Application shutting down")
        self._stop_slideshow_timer()
        Gtk.Application.do_shutdown(self)


class UiError(Exception):
    """Base exception for UI-related errors"""

    pass


class InitializationError(UiError):
    """Error during UI initialization"""

    pass


class RuntimeError(UiError):
    """Error during UI runtime"""

    pass


def run_app(config: FrameConfig, photo_loader: PhotoLoader) -> int:
    """Run the GTK4 application"""
    logger.info("Initializing GTK4 application")

    # Configure rendering based on configuration
    if config.rendering_type == "CPU":
        logger.info("Configuring for CPU (software) rendering")
        os.environ["GSK_RENDERER"] = "cairo"
        os.environ["GDK_RENDERING"] = "image"
    elif config.rendering_type == "GPU":
        logger.info("Configuring for GPU (hardware) rendering")
        # Let GTK4 use its default hardware-accelerated rendering
        # Remove any software rendering overrides if they exist
        if "GSK_RENDERER" in os.environ and os.environ["GSK_RENDERER"] == "cairo":
            del os.environ["GSK_RENDERER"]
        if "GDK_RENDERING" in os.environ and os.environ["GDK_RENDERING"] == "image":
            del os.environ["GDK_RENDERING"]
    else:
        logger.warning(
            f"Unknown rendering_type '{config.rendering_type}', defaulting to GPU"
        )

    try:
        app = PictureFrameApp(config, photo_loader)
        logger.info("Running GTK4 application")
        exit_code = app.run(sys.argv)

        if exit_code == 0:
            logger.info("GTK4 application exited successfully")
        else:
            logger.error(f"GTK4 application exited with error code: {exit_code}")

        return exit_code

    except Exception as e:
        logger.error(f"Failed to run GTK4 application: {e}")
        raise RuntimeError(f"UI runtime error: {e}") from e


def get_screen_dimensions() -> tuple[int, int]:
    """
    Get the primary screen dimensions

    Returns:
        Tuple of (width, height) in pixels
    """
    try:
        # For GTK4, we need to use the GDK display and monitor information
        display = Gdk.Display.get_default()
        if display is None:
            logger.warning("Could not get default display, using fallback dimensions")
            return 1920, 1080

        # Get the primary monitor
        monitors = display.get_monitors()
        if monitors.get_n_items() == 0:
            logger.warning("No monitors found, using fallback dimensions")
            return 1920, 1080

        monitor = monitors.get_item(0)  # Get first monitor
        if monitor is None:
            logger.warning("Could not get monitor, using fallback dimensions")
            return 1920, 1080

        # Get the geometry
        geometry = monitor.get_geometry()
        width = geometry.width
        height = geometry.height

        logger.info(f"Detected screen dimensions: {width}x{height}")
        return width, height

    except Exception as e:
        logger.warning(f"Failed to get screen dimensions, using fallback: {e}")
        return 1920, 1080
