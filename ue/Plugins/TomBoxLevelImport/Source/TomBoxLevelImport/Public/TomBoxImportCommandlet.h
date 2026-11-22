#pragma once

#include "CoreMinimal.h"
#include "Commandlets/Commandlet.h"
#include "TomBoxImportCommandlet.generated.h"

/**
 * Headless import:
 *   UnrealEditor-Cmd.exe <project> -run=TomBoxImport -center=X,Y -radius=M [-level=0|1|2]
 *       [-server=http://127.0.0.1:8765] [-content=/Game/TomBox] [-map=Name] [-notextures] [-nanite]
 *       [-surroundings=M] [-maxtex=N] [-nonormals] [-flipgreen] [-nolumen]
 */
UCLASS()
class UTomBoxImportCommandlet : public UCommandlet
{
	GENERATED_BODY()
public:
	virtual int32 Main(const FString& Params) override;
};
