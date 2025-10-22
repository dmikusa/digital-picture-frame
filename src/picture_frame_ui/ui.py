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
from pathlib import Path
from typing import Optional

try:
    import gi
    gi.require_version('Gtk', '4.0')
    gi.require_version('Gio', '2.0')
    gi.require_version('GLib', '2.0')
    from gi.repository import Gtk, Gio, GLib
except ImportError as e:
    print(f"Failed to import GTK4: {e}")
    print("Please install PyGObject and GTK4:")
    print("  uv add pygobject")
    print("  # Also ensure GTK4 is installed on your system")
    sys.exit(1)

from .config import FrameConfig
from .photos import PhotoLoader

logger = logging.getLogger(__name__)


class PictureFrameApp(Gtk.Application):
    """GTK4 application for the digital picture frame"""
    
    def __init__(self, config: FrameConfig, photo_loader: PhotoLoader):
        super().__init__(application_id="com.mikusa.picture-frame-ui")
        self.config = config
        self.photo_loader = photo_loader
        self.window: Optional[Gtk.ApplicationWindow] = None
        self.stack: Optional[Gtk.Stack] = None
        self.picture1: Optional[Gtk.Picture] = None
        self.picture2: Optional[Gtk.Picture] = None
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
        logger.debug(f"Application quit action set up with keyboard shortcut: {description} ({accelerator})")
    
    def _on_quit_action(self, action, parameter):
        """Handle quit action"""
        logger.debug("Quit action triggered from keyboard shortcut")
        self.quit()
    
    def do_activate(self):
        """Called when the application is activated"""
        logger.debug("App activation callback called")
        if self.window is None:
            self.build_ui()
        self.window.present()
    
    def build_ui(self):
        """Build the main UI components"""
        logger.debug("Building UI with crossfade animation")
        
        # Create the main application window
        self.window = Gtk.ApplicationWindow(application=self)
        self.window.set_title("Digital Picture Frame")
        self.window.set_default_size(800, 600)
        
        # Create a vertical box to hold our UI elements
        vbox = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        vbox.set_margin_top(12)
        vbox.set_margin_bottom(12)
        vbox.set_margin_start(12)
        vbox.set_margin_end(12)
        
        # Create a Stack widget for crossfade animation between images
        self.stack = Gtk.Stack()
        self.stack.set_transition_type(Gtk.StackTransitionType.CROSSFADE)
        self.stack.set_transition_duration(self.config.fade_duration)
        self.stack.set_hhomogeneous(True)
        self.stack.set_vhomogeneous(True)
        
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
    
    def _create_picture_widget(self) -> Gtk.Picture:
        """Create a configured Picture widget"""
        picture = Gtk.Picture()
        picture.set_can_shrink(True)
        picture.set_keep_aspect_ratio(True)
        picture.set_alternative_text("Digital Picture Frame Display")
        return picture
    
    def _load_image_into_picture(self, picture: Gtk.Picture):
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
        logger.debug(f"Slideshow timer started with {self.config.slideshow_duration}s interval")
    
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