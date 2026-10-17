/*
 * WordCraft's air/OS (Haiku) host: the application, its window and the system
 * services the Rust side reaches through the C functions at the end of this
 * file (bound in src/host.rs).
 *
 * Threads: be_app runs its message loop on the process's main thread
 * (wc_host_run), the window on its own looper thread, and the Rust interface
 * on a thread of its own. Input arrives in the window thread and is queued
 * here for the Rust thread, which waits on the queue between frames
 * (wc_host_wait) and hands back finished frames as 32-bit BGRA pixels
 * (wc_host_present). Every call from the Rust thread that touches the window
 * locks it first.
 */

#include <Alert.h>
#include <Application.h>
#include <Bitmap.h>
#include <Clipboard.h>
#include <Cursor.h>
#include <Directory.h>
#include <Entry.h>
#include <FilePanel.h>
#include <FindDirectory.h>
#include <Font.h>
#include <FormattingConventions.h>
#include <InterfaceDefs.h>
#include <Locale.h>
#include <LocaleRoster.h>
#include <Message.h>
#include <Messenger.h>
#include <Mime.h>
#include <Node.h>
#include <Path.h>
#include <Region.h>
#include <Roster.h>
#include <Screen.h>
#include <String.h>
#include <Url.h>
#include <View.h>
#include <Window.h>

#include <dlfcn.h>
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <condition_variable>
#include <chrono>
#include <deque>
#include <mutex>
#include <string>
#include <vector>


extern "C" {

// Keep in step with `Event` in src/host.rs.
struct WcEvent {
	int32_t kind;
	int32_t buttons;
	int32_t clicks;
	uint32_t modifiers;
	int32_t key;
	int32_t raw_char;
	int32_t repeat;
	int32_t id;
	float x;
	float y;
	float dx;
	float dy;
	uint32_t text_len;
};

struct WcWindowInfo {
	float inner_left, inner_top, inner_right, inner_bottom;
	float outer_left, outer_top, outer_right, outer_bottom;
	float screen_width, screen_height;
	int32_t minimized;
	int32_t active;
	int32_t zoomed;
};

}


enum {
	WC_MOUSE_MOVED = 1,
	WC_MOUSE_DOWN = 2,
	WC_MOUSE_UP = 3,
	WC_WHEEL = 4,
	WC_KEY_DOWN = 5,
	WC_KEY_UP = 6,
	WC_RESIZED = 7,
	WC_FOCUS = 8,
	WC_CLOSE = 9,
	WC_OPEN_FILES = 10,
	WC_DROPPED_FILES = 11,
	WC_POINTER_GONE = 12,
	WC_QUIT_REQUESTED = 13,
	WC_DIALOG_ANSWER = 14,
	WC_MODIFIERS = 15,
	WC_MOVED = 16,
	WC_MINIMIZED = 17,
	WC_COLORS = 18,
};

static const uint32 kDialogAnswer = 'wcDA';
static const char* kDialogField = "wc:dialog";


// #pragma mark - Event queue


struct QueuedEvent {
	WcEvent event;
	std::string text;
};

static std::mutex sQueueLock;
static std::condition_variable sQueueCondition;
static std::deque<QueuedEvent> sQueue;
static bool sWoken = false;
// The text of the event wc_host_next_event returned last.
static std::string sCurrentText;
static volatile bool sAllowQuit = false;


static WcEvent
MakeEvent(int32_t kind)
{
	WcEvent event;
	memset(&event, 0, sizeof(event));
	event.kind = kind;
	event.modifiers = modifiers();
	return event;
}


static void
Push(const WcEvent& event, const std::string& text = std::string())
{
	{
		std::lock_guard<std::mutex> lock(sQueueLock);
		// A burst of moves only needs the last position.
		if (event.kind == WC_MOUSE_MOVED && !sQueue.empty()
			&& sQueue.back().event.kind == WC_MOUSE_MOVED
			&& sQueue.back().event.buttons == event.buttons) {
			sQueue.back().event = event;
		} else {
			QueuedEvent queued;
			queued.event = event;
			queued.text = text;
			sQueue.push_back(queued);
		}
	}
	sQueueCondition.notify_all();
}


static std::string
PathsOf(const BMessage* message)
{
	std::string paths;
	entry_ref ref;
	for (int32 i = 0; message->FindRef("refs", i, &ref) == B_OK; i++) {
		BEntry entry(&ref, true);
		BPath path;
		if (entry.GetPath(&path) != B_OK)
			continue;
		if (!paths.empty())
			paths += '\n';
		paths += path.Path();
	}
	return paths;
}


// #pragma mark - Canvas view


class WcView : public BView {
public:
	WcView(BRect frame)
		:
		BView(frame, "canvas", B_FOLLOW_ALL_SIDES, B_WILL_DRAW | B_FRAME_EVENTS),
		fBitmap(NULL),
		fScale(1)
	{
		SetViewColor(B_TRANSPARENT_COLOR);
		SetDrawingMode(B_OP_COPY);
	}

	~WcView()
	{
		delete fBitmap;
	}

	virtual void Draw(BRect updateRect)
	{
		// The frame is in device pixels (fScale per logical pixel, on an
		// air/OS desktop drawn at a higher density): app_server scales the
		// logical destination back up, so it lands one to one.
		BRect painted;
		if (fBitmap != NULL) {
			BRect logical = fBitmap->Bounds();
			logical.right = (logical.right + 1) / fScale - 1;
			logical.bottom = (logical.bottom + 1) / fScale - 1;
			painted = logical & updateRect;
			if (painted.IsValid()) {
				BRect source(painted.left * fScale, painted.top * fScale,
					(painted.right + 1) * fScale - 1,
					(painted.bottom + 1) * fScale - 1);
				source = source & fBitmap->Bounds();
				DrawBitmapAsync(fBitmap, source, painted);
			}
		}
		// Uncovered parts (the window grew and the next frame isn't there
		// yet) get the panel colour instead of garbage.
		SetHighColor(ui_color(B_PANEL_BACKGROUND_COLOR));
		BRegion rest(updateRect);
		if (painted.IsValid())
			rest.Exclude(painted);
		FillRegion(&rest);
	}

	virtual void MouseDown(BPoint where)
	{
		MakeFocus(true);
		SetMouseEventMask(B_POINTER_EVENTS, B_NO_POINTER_HISTORY);
		WcEvent event = MakeEvent(WC_MOUSE_DOWN);
		event.x = where.x;
		event.y = where.y;
		BMessage* message = Window()->CurrentMessage();
		if (message != NULL) {
			message->FindInt32("buttons", &event.buttons);
			message->FindInt32("clicks", &event.clicks);
		}
		Push(event);
	}

	virtual void MouseUp(BPoint where)
	{
		WcEvent event = MakeEvent(WC_MOUSE_UP);
		event.x = where.x;
		event.y = where.y;
		BMessage* message = Window()->CurrentMessage();
		if (message != NULL)
			message->FindInt32("buttons", &event.buttons);
		Push(event);
	}

	virtual void MouseMoved(BPoint where, uint32 transit,
		const BMessage* dragMessage)
	{
		WcEvent event = MakeEvent(WC_MOUSE_MOVED);
		event.x = where.x;
		event.y = where.y;
		BMessage* message = Window()->CurrentMessage();
		if (message != NULL)
			message->FindInt32("buttons", &event.buttons);
		if (transit == B_EXITED_VIEW && event.buttons == 0)
			event.kind = WC_POINTER_GONE;
		Push(event);
	}

	virtual void KeyDown(const char* bytes, int32 numBytes)
	{
		Key(WC_KEY_DOWN, Window()->CurrentMessage(), bytes, numBytes);
	}

	virtual void KeyUp(const char* bytes, int32 numBytes)
	{
		Key(WC_KEY_UP, Window()->CurrentMessage(), bytes, numBytes);
	}

	void Key(int32 kind, BMessage* message, const char* bytes, int32 numBytes)
	{
		WcEvent event = MakeEvent(kind);
		if (message != NULL) {
			message->FindInt32("key", &event.key);
			message->FindInt32("raw_char", &event.raw_char);
			int32 modifiers;
			if (message->FindInt32("modifiers", &modifiers) == B_OK)
				event.modifiers = modifiers;
			int32 repeat;
			if (message->FindInt32("be:key_repeat", &repeat) == B_OK)
				event.repeat = repeat;
		}
		std::string text(bytes != NULL ? bytes : "",
			bytes != NULL && numBytes > 0 ? numBytes : 0);
		event.text_len = text.size();
		Push(event, text);
	}

	virtual void FrameResized(float width, float height)
	{
		WcEvent event = MakeEvent(WC_RESIZED);
		event.x = width + 1;
		event.y = height + 1;
		Push(event);
	}

	virtual void MessageReceived(BMessage* message)
	{
		if (message->WasDropped() && message->HasRef("refs")) {
			BPoint where = ConvertFromScreen(message->DropPoint());
			WcEvent event = MakeEvent(WC_DROPPED_FILES);
			event.x = where.x;
			event.y = where.y;
			std::string paths = PathsOf(message);
			event.text_len = paths.size();
			Push(event, paths);
			return;
		}
		switch (message->what) {
			case B_MOUSE_WHEEL_CHANGED:
			{
				WcEvent event = MakeEvent(WC_WHEEL);
				message->FindFloat("be:wheel_delta_x", &event.dx);
				message->FindFloat("be:wheel_delta_y", &event.dy);
				Push(event);
				break;
			}
			case B_MODIFIERS_CHANGED:
				Push(MakeEvent(WC_MODIFIERS));
				break;
			default:
				BView::MessageReceived(message);
		}
	}

	// Called with the window locked. The pixels are device pixels, scale
	// per logical pixel.
	void Present(const uint32_t* pixels, int32 width, int32 height,
		int32 x0, int32 y0, int32 x1, int32 y1, float scale)
	{
		if (scale != fScale) {
			fScale = scale;
			delete fBitmap;
			fBitmap = NULL;
		}
		if (fBitmap == NULL || fBitmap->Bounds().IntegerWidth() + 1 != width
			|| fBitmap->Bounds().IntegerHeight() + 1 != height) {
			delete fBitmap;
			fBitmap = new BBitmap(BRect(0, 0, width - 1, height - 1), B_RGB32);
			if (fBitmap->InitCheck() != B_OK) {
				delete fBitmap;
				fBitmap = NULL;
				return;
			}
			x0 = 0;
			y0 = 0;
			x1 = width;
			y1 = height;
		}
		if (x0 < 0)
			x0 = 0;
		if (y0 < 0)
			y0 = 0;
		if (x1 > width)
			x1 = width;
		if (y1 > height)
			y1 = height;
		if (x1 <= x0 || y1 <= y0)
			return;
		uint8* bits = (uint8*)fBitmap->Bits();
		int32 bytesPerRow = fBitmap->BytesPerRow();
		for (int32 y = y0; y < y1; y++) {
			memcpy(bits + (size_t)y * bytesPerRow + (size_t)x0 * 4,
				pixels + (size_t)y * width + x0, (size_t)(x1 - x0) * 4);
		}
		Invalidate(BRect(floorf(x0 / fScale), floorf(y0 / fScale),
			ceilf(x1 / fScale) - 1, ceilf(y1 / fScale) - 1));
	}

private:
	BBitmap* fBitmap;
	float fScale;
};


// #pragma mark - Window


class WcWindow : public BWindow {
public:
	WcWindow(BRect frame, const char* title)
		:
		BWindow(frame, title, B_TITLED_WINDOW, B_ASYNCHRONOUS_CONTROLS)
	{
		fView = new WcView(Bounds());
		AddChild(fView);
		fView->MakeFocus(true);
		// The interface has its own Cut, Copy, Paste, Select All and Close
		// (with a Save Changes prompt), so these keys reach it as keys.
		RemoveShortcut('W', B_COMMAND_KEY);
		RemoveShortcut('X', B_COMMAND_KEY);
		RemoveShortcut('C', B_COMMAND_KEY);
		RemoveShortcut('V', B_COMMAND_KEY);
		RemoveShortcut('A', B_COMMAND_KEY);
	}

	virtual bool QuitRequested()
	{
		if (sAllowQuit)
			return true;
		Push(MakeEvent(WC_CLOSE));
		return false;
	}

	virtual void DispatchMessage(BMessage* message, BHandler* handler)
	{
		// BWindow eats every Command+key it has no shortcut for; the
		// interface's shortcuts (Command+B for bold…) need them. Command+Q
		// still quits the Haiku way.
		if (message->what == B_KEY_DOWN) {
			int32 modifiers = 0;
			int32 rawChar = 0;
			const char* bytes = NULL;
			message->FindInt32("modifiers", &modifiers);
			message->FindInt32("raw_char", &rawChar);
			message->FindString("bytes", &bytes);
			if ((modifiers & B_COMMAND_KEY) != 0 && rawChar != 'q'
				&& rawChar != 'Q') {
				fView->Key(WC_KEY_DOWN, message, bytes,
					bytes != NULL ? strlen(bytes) : 0);
				return;
			}
		}
		BWindow::DispatchMessage(message, handler);
	}

	virtual void WindowActivated(bool active)
	{
		WcEvent event = MakeEvent(WC_FOCUS);
		event.x = active ? 1 : 0;
		Push(event);
	}

	virtual void FrameMoved(BPoint where)
	{
		WcEvent event = MakeEvent(WC_MOVED);
		event.x = where.x;
		event.y = where.y;
		Push(event);
	}

	virtual void Minimize(bool minimize)
	{
		BWindow::Minimize(minimize);
		WcEvent event = MakeEvent(WC_MINIMIZED);
		event.x = minimize ? 1 : 0;
		Push(event);
	}

	virtual void ScreenChanged(BRect frame, color_space mode)
	{
		Push(MakeEvent(WC_COLORS));
	}

	virtual void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case B_COLORS_UPDATED:
				Push(MakeEvent(WC_COLORS));
				break;
			default:
				BWindow::MessageReceived(message);
		}
	}

	WcView* View() const { return fView; }

private:
	WcView* fView;
};


static WcWindow* sWindow = NULL;


// The frame of the monitor most of the window is on. On air/OS one desktop
// spans every monitor (BScreen's frame is all of them); its display layout
// knows each one. Elsewhere, the screen.
static BRect
MonitorFrame(BWindow* window)
{
	typedef status_t (*GetDisplayFrame)(BRect, bool, BRect&);
	static GetDisplayFrame getFrame = (GetDisplayFrame)dlsym(RTLD_DEFAULT,
		"_ZN8BPrivate17get_display_frameE5BRectbRS0_");
	BRect display;
	if (getFrame != NULL && getFrame(window->Frame(), false, display) == B_OK
		&& display.IsValid())
		return display;
	return BScreen(window).Frame();
}


// #pragma mark - File panels


// Shows the files an Open panel offers: folders, and files with one of the
// extensions (all files when there are none).
class ExtensionFilter : public BRefFilter {
public:
	ExtensionFilter(const char* extensions)
	{
		BString list(extensions != NULL ? extensions : "");
		int32 start = 0;
		while (start <= list.Length()) {
			int32 end = list.FindFirst('\n', start);
			if (end < 0)
				end = list.Length();
			BString extension;
			list.CopyInto(extension, start, end - start);
			extension.ToLower();
			if (extension == "*") {
				fExtensions.clear();
				fAll = true;
				return;
			}
			if (extension.Length() > 0)
				fExtensions.push_back(extension);
			start = end + 1;
		}
		fAll = fExtensions.empty();
	}

	virtual bool Filter(const entry_ref* ref, BNode* node,
		struct stat_beos* stat, const char* mimeType)
	{
		if (fAll)
			return true;
		BEntry entry(ref, true);
		if (entry.IsDirectory())
			return true;
		BString name(ref->name);
		name.ToLower();
		for (size_t i = 0; i < fExtensions.size(); i++) {
			BString suffix(".");
			suffix << fExtensions[i];
			if (name.EndsWith(suffix))
				return true;
		}
		return false;
	}

private:
	std::vector<BString> fExtensions;
	bool fAll;
};


static BFilePanel* sOpenPanel = NULL;
static ExtensionFilter* sOpenFilter = NULL;
static BFilePanel* sSavePanel = NULL;
static int32 sDialogId = -1;
static bool sDialogAnswered = true;


static void
AnswerDialog(int32 id, bool picked, const std::string& path)
{
	if (id != sDialogId || sDialogAnswered)
		return;
	sDialogAnswered = true;
	WcEvent event = MakeEvent(WC_DIALOG_ANSWER);
	event.id = id;
	event.x = picked ? 1 : 0;
	event.text_len = path.size();
	Push(event, path);
}


// #pragma mark - Application


class WcApp : public BApplication {
public:
	WcApp(const char* signature)
		:
		BApplication(signature)
	{
	}

	virtual void RefsReceived(BMessage* message)
	{
		int32 dialog;
		if (message->FindInt32(kDialogField, &dialog) == B_OK) {
			std::string path = PathsOf(message);
			AnswerDialog(dialog, !path.empty(), path);
			return;
		}
		WcEvent event = MakeEvent(WC_OPEN_FILES);
		std::string paths = PathsOf(message);
		event.text_len = paths.size();
		if (!paths.empty())
			Push(event, paths);
	}

	virtual void ArgvReceived(int32 argc, char** argv)
	{
		// The Rust side reads its own command line.
	}

	virtual bool QuitRequested()
	{
		if (sAllowQuit)
			return BApplication::QuitRequested();
		Push(MakeEvent(WC_QUIT_REQUESTED));
		return false;
	}

	virtual void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case B_SAVE_REQUESTED:
			{
				int32 dialog;
				entry_ref directory;
				const char* name;
				if (message->FindInt32(kDialogField, &dialog) != B_OK)
					break;
				if (message->FindRef("directory", &directory) != B_OK
					|| message->FindString("name", &name) != B_OK) {
					AnswerDialog(dialog, false, std::string());
					break;
				}
				BPath path(&directory);
				path.Append(name);
				AnswerDialog(dialog, path.InitCheck() == B_OK,
					path.InitCheck() == B_OK ? path.Path() : "");
				break;
			}
			case B_CANCEL:
				// Sent whenever a panel closes, after its answer if it had
				// one.
				AnswerDialog(sDialogId, false, std::string());
				break;
			default:
				BApplication::MessageReceived(message);
		}
	}
};


// #pragma mark - Clipboard


static void*
CopyOut(const void* data, size_t length, size_t* _length)
{
	void* copy = malloc(length > 0 ? length : 1);
	if (copy == NULL)
		return NULL;
	memcpy(copy, data, length);
	*_length = length;
	return copy;
}


// #pragma mark - C interface


extern "C" {


int32_t
wc_host_init(const char* signature)
{
	if (be_app != NULL)
		return 0;
	new WcApp(signature);
	return be_app != NULL && be_app->InitCheck() == B_OK ? 0 : -1;
}


// Opens the window: its frame's top-left corner at (x, y) (centred on the
// screen when x is negative), content size width × height.
int32_t
wc_host_open_window(const char* title, float x, float y, float width,
	float height, float minWidth, float minHeight, int32_t zoomed)
{
	if (sWindow != NULL)
		return 0;
	BRect frame(0, 0, width - 1, height - 1);
	BScreen screen;
	BRect screenFrame = screen.Frame();
	if (x >= 0 && y >= 0)
		frame.OffsetTo(x, y);
	sWindow = new WcWindow(frame, title);
	sWindow->SetSizeLimits(minWidth - 1, 32768, minHeight - 1, 32768);
	if (x < 0 || y < 0 || !screenFrame.Intersects(frame))
		sWindow->CenterOnScreen();
	else {
		// (x, y) is where the frame's corner goes, tab and border included.
		BRect outer = sWindow->DecoratorFrame();
		sWindow->MoveBy(frame.left - outer.left, frame.top - outer.top);
	}
	if (zoomed)
		sWindow->Zoom();
	sWindow->Show();
	return 0;
}


void
wc_host_run()
{
	be_app->Run();
	delete be_app;
}


void
wc_host_quit()
{
	sAllowQuit = true;
	be_app->PostMessage(B_QUIT_REQUESTED);
}


// Waits until an event is queued, wc_host_wake is called or the timeout
// (microseconds; negative: none) passes. Returns how many events are queued.
int32_t
wc_host_wait(int64_t timeout)
{
	std::unique_lock<std::mutex> lock(sQueueLock);
	auto ready = [] { return !sQueue.empty() || sWoken; };
	if (timeout < 0)
		sQueueCondition.wait(lock, ready);
	else if (timeout > 0)
		sQueueCondition.wait_for(lock, std::chrono::microseconds(timeout), ready);
	sWoken = false;
	return (int32_t)sQueue.size();
}


void
wc_host_wake()
{
	{
		std::lock_guard<std::mutex> lock(sQueueLock);
		sWoken = true;
	}
	sQueueCondition.notify_all();
}


// Takes the next event, if any (1) or returns 0. Its text stays readable
// through wc_host_event_text until the next call.
int32_t
wc_host_next_event(WcEvent* event)
{
	std::lock_guard<std::mutex> lock(sQueueLock);
	if (sQueue.empty())
		return 0;
	*event = sQueue.front().event;
	sCurrentText = sQueue.front().text;
	event->text_len = sCurrentText.size();
	sQueue.pop_front();
	return 1;
}


const char*
wc_host_event_text()
{
	return sCurrentText.c_str();
}


void
wc_host_present(const uint32_t* pixels, int32_t width, int32_t height,
	int32_t x0, int32_t y0, int32_t x1, int32_t y1, float scale)
{
	if (sWindow == NULL || width <= 0 || height <= 0 || scale <= 0
		|| !sWindow->Lock())
		return;
	sWindow->View()->Present(pixels, width, height, x0, y0, x1, y1, scale);
	sWindow->Unlock();
}


void
wc_host_view_size(float* width, float* height)
{
	*width = 0;
	*height = 0;
	if (sWindow == NULL || !sWindow->Lock())
		return;
	BRect bounds = sWindow->View()->Bounds();
	*width = bounds.IntegerWidth() + 1;
	*height = bounds.IntegerHeight() + 1;
	sWindow->Unlock();
}


void
wc_host_window_info(WcWindowInfo* info)
{
	memset(info, 0, sizeof(*info));
	if (sWindow == NULL || !sWindow->Lock())
		return;
	BRect inner = sWindow->Frame();
	BRect outer = sWindow->DecoratorFrame();
	BRect screen = MonitorFrame(sWindow);
	info->inner_left = inner.left;
	info->inner_top = inner.top;
	info->inner_right = inner.right + 1;
	info->inner_bottom = inner.bottom + 1;
	info->outer_left = outer.left;
	info->outer_top = outer.top;
	info->outer_right = outer.right + 1;
	info->outer_bottom = outer.bottom + 1;
	info->screen_width = screen.Width() + 1;
	info->screen_height = screen.Height() + 1;
	info->minimized = sWindow->IsMinimized();
	info->active = sWindow->IsActive();
	// Haiku doesn't say whether a window is zoomed.
	info->zoomed = 0;
	sWindow->Unlock();
}


void
wc_host_set_title(const char* title)
{
	if (sWindow == NULL || !sWindow->Lock())
		return;
	if (strcmp(sWindow->Title(), title) != 0)
		sWindow->SetTitle(title);
	sWindow->Unlock();
}


// The window commands of src/host.rs's `WindowCommand`.
void
wc_host_window_command(int32_t command, float a, float b)
{
	if (sWindow == NULL || !sWindow->Lock())
		return;
	switch (command) {
		case 1:
		{
			// Move the frame's top-left corner (decorator included) to (a, b).
			BRect inner = sWindow->Frame();
			BRect outer = sWindow->DecoratorFrame();
			sWindow->MoveTo(a + inner.left - outer.left, b + inner.top - outer.top);
			break;
		}
		case 2:
			sWindow->ResizeTo(a - 1, b - 1);
			break;
		case 3:
			sWindow->Minimize(a != 0);
			break;
		case 4:
			// Zoom toggles: only zoom, never un-zoom on request.
			if (a != 0)
				sWindow->Zoom();
			break;
		case 5:
			sWindow->Activate(true);
			break;
		case 6:
		{
			float minWidth, maxWidth, minHeight, maxHeight;
			sWindow->GetSizeLimits(&minWidth, &maxWidth, &minHeight, &maxHeight);
			sWindow->SetSizeLimits(a - 1, maxWidth, b - 1, maxHeight);
			break;
		}
	}
	sWindow->Unlock();
}


void
wc_host_set_cursor(int32_t id)
{
	static int32_t sCurrent = -1;
	static BCursor* sCursors[32] = {};
	if (id < 1 || id > 31 || sWindow == NULL)
		id = B_CURSOR_ID_SYSTEM_DEFAULT;
	if (id == sCurrent)
		return;
	if (sCursors[id] == NULL)
		sCursors[id] = new BCursor((BCursorID)id);
	if (!sWindow->Lock())
		return;
	sWindow->View()->SetViewCursor(sCursors[id]);
	sWindow->Unlock();
	sCurrent = id;
}


uint32_t
wc_host_modifiers()
{
	return modifiers();
}


// The interface scale: the system font size against Haiku's default 12 pt.
float
wc_host_ui_scale()
{
	float scale = be_plain_font->Size() / 12.0f;
	if (scale < 1.0f)
		scale = 1.0f;
	if (scale > 4.0f)
		scale = 4.0f;
	return scale;
}


// How many frame buffer pixels a logical pixel is: air/OS draws the desktop
// at its monitors' density (the "render scale" of its display layout, in
// percent). 1 on a Haiku without the call.
float
wc_host_device_scale()
{
	typedef status_t (*GetLayout)(BMessage&);
	static GetLayout getLayout = (GetLayout)dlsym(RTLD_DEFAULT,
		"_ZN8BPrivate18get_display_layoutER8BMessage");
	if (getLayout == NULL)
		return 1;
	BMessage layout;
	if (getLayout(layout) != B_OK)
		return 1;
	int32 best = 0;
	BMessage display;
	for (int32 i = 0; layout.FindMessage("display", i, &display) == B_OK; i++) {
		int32 scale;
		if (display.GetBool("enabled", true)
			&& display.FindInt32("render scale", &scale) == B_OK && scale > best)
			best = scale;
	}
	if (best < 100 || best > 400)
		return 1;
	return best / 100.0f;
}


// Whether the panel colour (Appearance preferences) is dark.
int32_t
wc_host_dark()
{
	rgb_color color = ui_color(B_PANEL_BACKGROUND_COLOR);
	return color.Brightness() < 128 ? 1 : 0;
}


// The system's formatting conventions (en_US) and the preferred languages
// (en, de…, one per line), for the locale variables the interface reads.
char*
wc_host_locale(char** languages)
{
	BFormattingConventions conventions;
	BString id;
	BLocaleRoster* roster = BLocaleRoster::Default();
	if (roster != NULL) {
		const BLocale* locale = roster->GetDefaultLocale();
		if (locale != NULL && locale->GetFormattingConventions(&conventions) == B_OK)
			id = conventions.ID();
	}
	BString list;
	BMessage preferred;
	if (roster != NULL && roster->GetPreferredLanguages(&preferred) == B_OK) {
		const char* language;
		for (int32 i = 0; preferred.FindString("language", i, &language) == B_OK; i++) {
			if (list.Length() > 0)
				list << "\n";
			list << language;
		}
	}
	*languages = strdup(list.String());
	return strdup(id.String());
}


void
wc_free(void* data)
{
	free(data);
}


// The clipboard's data of one MIME type (malloc'd; NULL when absent).
void*
wc_clipboard_get(const char* type, size_t* length)
{
	*length = 0;
	void* copy = NULL;
	if (!be_clipboard->Lock())
		return NULL;
	BMessage* clip = be_clipboard->Data();
	const void* data;
	ssize_t size;
	if (clip != NULL && clip->FindData(type, B_MIME_TYPE, &data, &size) == B_OK
		&& size >= 0)
		copy = CopyOut(data, size, length);
	be_clipboard->Unlock();
	return copy;
}


// The names of the clipboard's data, one per line (malloc'd).
char*
wc_clipboard_formats()
{
	BString list;
	if (be_clipboard->Lock()) {
		BMessage* clip = be_clipboard->Data();
		char* name;
		type_code type;
		for (int32 i = 0; clip != NULL
			&& clip->GetInfo(B_ANY_TYPE, i, &name, &type) == B_OK; i++) {
			if (list.Length() > 0)
				list << "\n";
			list << name;
		}
		be_clipboard->Unlock();
	}
	return strdup(list.String());
}


// Replaces the clipboard with text and, when html isn't NULL, its HTML.
int32_t
wc_clipboard_set(const char* text, size_t textLength, const char* html,
	size_t htmlLength)
{
	if (!be_clipboard->Lock())
		return -1;
	be_clipboard->Clear();
	BMessage* clip = be_clipboard->Data();
	status_t status = B_ERROR;
	if (clip != NULL) {
		status = clip->AddData("text/plain", B_MIME_TYPE, text, textLength);
		if (status == B_OK && html != NULL)
			status = clip->AddData("text/html", B_MIME_TYPE, html, htmlLength);
		if (status == B_OK)
			status = be_clipboard->Commit();
	}
	be_clipboard->Unlock();
	return status == B_OK ? 0 : -1;
}


// Puts an encoded picture (PNG) on the clipboard.
int32_t
wc_clipboard_set_picture(const char* type, const void* data, size_t length)
{
	if (!be_clipboard->Lock())
		return -1;
	be_clipboard->Clear();
	BMessage* clip = be_clipboard->Data();
	status_t status = B_ERROR;
	if (clip != NULL) {
		status = clip->AddData(type, B_MIME_TYPE, data, length);
		if (status == B_OK)
			status = be_clipboard->Commit();
	}
	be_clipboard->Unlock();
	return status == B_OK ? 0 : -1;
}


// A picture another Haiku program copied as an archived BBitmap
// ("image/bitmap", as ShowImage and screenshot tools do), as straight RGBA
// (malloc'd; NULL when there's none).
void*
wc_clipboard_bitmap(int32_t* width, int32_t* height)
{
	*width = 0;
	*height = 0;
	BMessage archive;
	bool found = false;
	if (be_clipboard->Lock()) {
		BMessage* clip = be_clipboard->Data();
		found = clip != NULL && clip->FindMessage("image/bitmap", &archive) == B_OK;
		be_clipboard->Unlock();
	}
	if (!found)
		return NULL;
	BBitmap source(&archive);
	if (source.InitCheck() != B_OK)
		return NULL;
	BRect bounds = source.Bounds();
	int32 w = bounds.IntegerWidth() + 1;
	int32 h = bounds.IntegerHeight() + 1;
	if (w <= 0 || h <= 0 || w > 16000 || h > 16000)
		return NULL;
	BBitmap converted(bounds, B_RGBA32);
	if (converted.InitCheck() != B_OK || converted.ImportBits(&source) != B_OK)
		return NULL;
	uint8* rgba = (uint8*)malloc((size_t)w * h * 4);
	if (rgba == NULL)
		return NULL;
	bool opaque = source.ColorSpace() == B_RGB32 || source.ColorSpace() == B_RGB24
		|| source.ColorSpace() == B_RGB16 || source.ColorSpace() == B_CMAP8;
	const uint8* bits = (const uint8*)converted.Bits();
	for (int32 y = 0; y < h; y++) {
		const uint8* row = bits + (size_t)y * converted.BytesPerRow();
		uint8* out = rgba + (size_t)y * w * 4;
		for (int32 x = 0; x < w; x++) {
			out[x * 4 + 0] = row[x * 4 + 2];
			out[x * 4 + 1] = row[x * 4 + 1];
			out[x * 4 + 2] = row[x * 4 + 0];
			out[x * 4 + 3] = opaque ? 255 : row[x * 4 + 3];
		}
	}
	*width = w;
	*height = h;
	return rgba;
}


// Shows a file panel: Open (save == 0) offering the extensions (one per line;
// "*": any file), or Save suggesting the name. Its answer arrives as a
// WC_DIALOG_ANSWER event with the id.
int32_t
wc_host_file_panel(int32_t id, int32_t save, const char* name,
	const char* extensions, const char* title)
{
	BMessenger target(be_app);
	if (save) {
		if (sSavePanel == NULL) {
			sSavePanel = new BFilePanel(B_SAVE_PANEL, &target, NULL, 0, false,
				NULL, NULL, false, true);
		}
		BMessage message(B_SAVE_REQUESTED);
		message.AddInt32(kDialogField, id);
		sSavePanel->SetMessage(&message);
		sSavePanel->SetSaveText(name != NULL ? name : "");
		if (sSavePanel->Window()->Lock()) {
			sSavePanel->Window()->SetTitle(title);
			sSavePanel->Window()->Unlock();
		}
		sDialogId = id;
		sDialogAnswered = false;
		sSavePanel->Show();
		return 0;
	}
	if (sOpenPanel == NULL) {
		sOpenPanel = new BFilePanel(B_OPEN_PANEL, &target, NULL, B_FILE_NODE,
			false, NULL, NULL, false, true);
	}
	ExtensionFilter* filter = new ExtensionFilter(extensions);
	sOpenPanel->SetRefFilter(filter);
	delete sOpenFilter;
	sOpenFilter = filter;
	BMessage message(B_REFS_RECEIVED);
	message.AddInt32(kDialogField, id);
	sOpenPanel->SetMessage(&message);
	if (sOpenPanel->Window()->Lock()) {
		sOpenPanel->Window()->SetTitle(title);
		sOpenPanel->Window()->Unlock();
	}
	sOpenPanel->Refresh();
	sDialogId = id;
	sDialogAnswered = false;
	sOpenPanel->Show();
	return 0;
}


int32_t
wc_host_open_url(const char* url)
{
	BUrl target(url, true);
	if (!target.IsValid())
		return -1;
	return target.OpenWithPreferredApplication(false) == B_OK ? 0 : -1;
}


// Makes the application the preferred one for the document MIME types (one
// per line). Returns how many it set, or -1 when none could be.
int32_t
wc_host_make_default(const char* signature, const char* types)
{
	BString list(types);
	int32 set = 0;
	int32 start = 0;
	while (start < list.Length()) {
		int32 end = list.FindFirst('\n', start);
		if (end < 0)
			end = list.Length();
		BString type;
		list.CopyInto(type, start, end - start);
		start = end + 1;
		BMimeType mime(type.String());
		if (mime.InitCheck() != B_OK)
			continue;
		if (!mime.IsInstalled())
			mime.Install();
		if (mime.SetPreferredApp(signature) == B_OK)
			set++;
	}
	return set > 0 ? set : -1;
}


// Makes the application the preferred one for the types (one per line) that
// have none yet, so Tracker opens Word documents with it out of the box; a
// preferred application the user chose stays.
int32_t
wc_host_claim_types(const char* signature, const char* types)
{
	BString list(types);
	int32 claimed = 0;
	int32 start = 0;
	while (start < list.Length()) {
		int32 end = list.FindFirst('\n', start);
		if (end < 0)
			end = list.Length();
		BString type;
		list.CopyInto(type, start, end - start);
		start = end + 1;
		BMimeType mime(type.String());
		if (mime.InitCheck() != B_OK)
			continue;
		char preferred[B_MIME_TYPE_LENGTH];
		if (mime.IsInstalled() && mime.GetPreferredApp(preferred) == B_OK
			&& preferred[0] != '\0')
			continue;
		if (!mime.IsInstalled())
			mime.Install();
		if (mime.SetPreferredApp(signature) == B_OK)
			claimed++;
	}
	return claimed;
}


// Gives a file the system saved for us its MIME type (by extension and
// content), as files saved by Haiku programs have, so Tracker opens it with
// the right application. Leaves a type already set alone.
int32_t
wc_host_type_file(const char* path)
{
	return update_mime_info(path, false, true, B_UPDATE_MIME_INFO_NO_FORCE)
		>= B_OK ? 0 : -1;
}


void
wc_host_alert(const char* title, const char* text)
{
	BAlert* alert = new BAlert(title, text, "OK", NULL, NULL,
		B_WIDTH_AS_USUAL, B_STOP_ALERT);
	alert->Go();
}


}
