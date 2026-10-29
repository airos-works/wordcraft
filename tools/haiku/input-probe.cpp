/*
 * input-probe: sends WordCraft's window the input that a test machine
 * without the hardware or add-ons can't produce, and shows what reaches the
 * clipboard (docs/haiku.md, Testing).
 *
 *   input-probe TEAM ime [SECONDS [PREEDIT COMMIT]]
 *                                    an input method's composition: started,
 *                                    a pre-edit (default "nihongo" in kana)
 *                                    with its first two characters selected,
 *                                    a location request (its answer printed),
 *                                    SECONDS to look at it, then confirmed
 *                                    (default "nihongo" in kanji)
 *   input-probe TEAM ime-cancel [SECONDS [PREEDIT]]
 *                                    the same, stopped without confirming
 *   input-probe TEAM point X Y       the pointer at (X, Y) in the canvas
 *   input-probe TEAM touchpad DY [N] an air/OS touchpad scroll gesture of DY
 *                                    wheel steps in N messages (default 20)
 *   input-probe TEAM wheel DY        a mouse wheel turned DY steps
 *   input-probe clipboard            the clipboard's types and sizes, and
 *                                    the text of its MathML types
 *   input-probe open APP FILE...     opens the files with the application
 *                                    file APP as Tracker does (BRoster's
 *                                    Launch with B_REFS_RECEIVED): a running
 *                                    single-launch copy gets them
 *
 * TEAM is the running copy's team id (several copies of an application can
 * run from different files). Messages go to its canvas view by scripting
 * (View "canvas" of Window 0), as the input server's would reach it.
 *
 * Build on air/OS: g++ -o input-probe input-probe.cpp -lbe
 */

#include <Application.h>
#include <Clipboard.h>
#include <Entry.h>
#include <Input.h>
#include <Looper.h>
#include <Message.h>
#include <Messenger.h>
#include <OS.h>
#include <Roster.h>
#include <View.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>


static BMessenger sTarget;


static void
Send(BMessage& message)
{
	message.AddSpecifier("View", "canvas");
	message.AddSpecifier("Window", (int32)0);
	status_t status = sTarget.SendMessage(&message, (BHandler*)NULL, 1000000);
	if (status != B_OK)
		fprintf(stderr, "send: %s\n", strerror(status));
}


// The input method's side of the transaction: prints what the application
// answers (locations, a stop it asks for).
class Method : public BLooper {
public:
	Method()
		:
		BLooper("input method")
	{
	}

	virtual void MessageReceived(BMessage* message)
	{
		if (message->what != B_INPUT_METHOD_EVENT) {
			BLooper::MessageReceived(message);
			return;
		}
		int32 opcode = -1;
		message->FindInt32("be:opcode", &opcode);
		if (opcode == B_INPUT_METHOD_LOCATION_REQUEST) {
			BPoint where;
			float height;
			for (int32 i = 0; message->FindPoint("be:location_reply", i, &where)
					== B_OK; i++) {
				message->FindFloat("be:height_reply", i, &height);
				printf("location %ld: %.1f, %.1f height %.1f\n", (long)i,
					where.x, where.y, height);
			}
		} else if (opcode == B_INPUT_METHOD_STOPPED)
			printf("the application stopped the input method\n");
		else
			printf("input method event %ld\n", (long)opcode);
		fflush(stdout);
	}
};


static void
InputMethod(bool confirm, int seconds, const char* composing,
	const char* result)
{
	Method* method = new Method;
	method->Run();
	BMessenger replyTo(method);

	BMessage started(B_INPUT_METHOD_EVENT);
	started.AddInt32("be:opcode", B_INPUT_METHOD_STARTED);
	started.AddMessenger("be:reply_to", replyTo);
	Send(started);
	snooze(100000);

	// The first two characters are the clause being converted (bytes).
	int32 selected = 0;
	for (int32 chars = 0; composing[selected] != '\0'; selected++) {
		if (((uint8)composing[selected] & 0xc0) != 0x80 && chars++ == 2)
			break;
	}
	BMessage changed(B_INPUT_METHOD_EVENT);
	changed.AddInt32("be:opcode", B_INPUT_METHOD_CHANGED);
	changed.AddString("be:string", composing);
	changed.AddInt32("be:selection", 0);
	changed.AddInt32("be:selection", selected);
	changed.AddInt32("be:clause_start", 0);
	changed.AddInt32("be:clause_end", (int32)strlen(composing));
	changed.AddBool("be:confirmed", false);
	Send(changed);
	printf("pre-edit sent\n");
	fflush(stdout);
	snooze(300000);

	BMessage location(B_INPUT_METHOD_EVENT);
	location.AddInt32("be:opcode", B_INPUT_METHOD_LOCATION_REQUEST);
	Send(location);
	snooze(500000);

	snooze((bigtime_t)seconds * 1000000);

	if (confirm) {
		BMessage confirmed(B_INPUT_METHOD_EVENT);
		confirmed.AddInt32("be:opcode", B_INPUT_METHOD_CHANGED);
		confirmed.AddString("be:string", result);
		confirmed.AddBool("be:confirmed", true);
		Send(confirmed);
		printf("confirmed\n");
	}
	BMessage stopped(B_INPUT_METHOD_EVENT);
	stopped.AddInt32("be:opcode", B_INPUT_METHOD_STOPPED);
	Send(stopped);
	printf("stopped\n");
	fflush(stdout);
	snooze(300000);
	if (method->Lock())
		method->Quit();
}


static void
Point(float x, float y)
{
	BMessage moved(B_MOUSE_MOVED);
	moved.AddPoint("be:view_where", BPoint(x, y));
	moved.AddInt32("be:transit", B_INSIDE_VIEW);
	moved.AddInt32("buttons", 0);
	moved.AddInt64("when", system_time());
	Send(moved);
}


static void
Wheel(float dy, int32 subtype, int32 phase, bool precise)
{
	BMessage wheel(B_MOUSE_WHEEL_CHANGED);
	wheel.AddInt64("when", system_time());
	wheel.AddInt32("be:device_subtype", subtype);
	wheel.AddFloat("be:wheel_delta_x", 0);
	wheel.AddFloat("be:wheel_delta_y", precise ? 0 : dy);
	if (precise) {
		wheel.AddFloat("be:wheel_precise_delta_x", 0);
		wheel.AddFloat("be:wheel_precise_delta_y", dy);
		wheel.AddInt32("be:wheel_phase", phase);
	}
	Send(wheel);
}


static void
Touchpad(float steps, int32 count)
{
	if (count < 1)
		count = 1;
	// air/OS's mouse add-on: began with the first movement, changed while
	// it goes on (whole steps aren't reported apart), ended with no delta.
	for (int32 i = 0; i < count; i++) {
		Wheel(steps / count, B_TOUCHPAD_POINTING_DEVICE, i == 0 ? 1 : 2,
			true);
		snooze(8000);
	}
	Wheel(0, B_TOUCHPAD_POINTING_DEVICE, 3, true);
}


static void
Clipboard()
{
	if (!be_clipboard->Lock())
		return;
	BMessage* clip = be_clipboard->Data();
	char* name;
	type_code type;
	for (int32 i = 0; clip != NULL
		&& clip->GetInfo(B_ANY_TYPE, i, &name, &type) == B_OK; i++) {
		const void* data;
		ssize_t size = 0;
		clip->FindData(name, type, &data, &size);
		printf("%s: %ld bytes\n", name, (long)size);
		if (strstr(name, "mathml") != NULL)
			printf("  %.*s\n", (int)size, (const char*)data);
	}
	be_clipboard->Unlock();
}


static int
Open(const char* app, char** files, int count)
{
	entry_ref appRef;
	if (get_ref_for_path(app, &appRef) != B_OK) {
		fprintf(stderr, "no application %s\n", app);
		return 1;
	}
	BMessage refs(B_REFS_RECEIVED);
	for (int i = 0; i < count; i++) {
		entry_ref ref;
		if (get_ref_for_path(files[i], &ref) == B_OK)
			refs.AddRef("refs", &ref);
	}
	team_id team = -1;
	status_t status = be_roster->Launch(&appRef, &refs, &team);
	printf("launch: %s, team %ld\n", strerror(status), (long)team);
	return status == B_OK || status == B_ALREADY_RUNNING ? 0 : 1;
}


int
main(int argc, char** argv)
{
	BApplication app("application/x-vnd.airos-input-probe");
	if (argc >= 2 && strcmp(argv[1], "clipboard") == 0) {
		Clipboard();
		return 0;
	}
	if (argc >= 3 && strcmp(argv[1], "open") == 0)
		return Open(argv[2], argv + 3, argc - 3);
	if (argc < 3) {
		fprintf(stderr, "usage: input-probe TEAM ime|ime-cancel [SECONDS"
			" [PREEDIT [COMMIT]]]"
			" | point X Y | touchpad DY [N] | wheel DY\n"
			"       input-probe clipboard | open APP FILE...\n");
		return 2;
	}
	sTarget = BMessenger(NULL, (team_id)atol(argv[1]));
	if (!sTarget.IsValid()) {
		fprintf(stderr, "no application with team %s\n", argv[1]);
		return 1;
	}
	const char* what = argv[2];
	if (strcmp(what, "ime") == 0 || strcmp(what, "ime-cancel") == 0) {
		// "nihongo" in kana while composing, in kanji confirmed.
		InputMethod(strcmp(what, "ime") == 0, argc > 3 ? atoi(argv[3]) : 0,
			argc > 4 ? argv[4]
				: "\xe3\x81\xab\xe3\x81\xbb\xe3\x82\x93\xe3\x81\x94",
			argc > 5 ? argv[5] : "\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e");
	}
	else if (strcmp(what, "point") == 0 && argc > 4)
		Point(atof(argv[3]), atof(argv[4]));
	else if (strcmp(what, "touchpad") == 0 && argc > 3)
		Touchpad(atof(argv[3]), argc > 4 ? atoi(argv[4]) : 20);
	else if (strcmp(what, "wheel") == 0 && argc > 3)
		Wheel(atof(argv[3]), B_MOUSE_POINTING_DEVICE, 0, false);
	else {
		fprintf(stderr, "unknown request %s\n", what);
		return 2;
	}
	return 0;
}
